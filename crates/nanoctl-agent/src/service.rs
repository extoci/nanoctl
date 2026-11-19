use crate::{config::AgentConfig, service_control::ServiceControl};
use anyhow::{Context, Result};
use std::{path::PathBuf, time::Duration};

pub async fn run(
    config: AgentConfig,
    ready_file: Option<PathBuf>,
    ready_token: Option<String>,
) -> Result<()> {
    let Some(control) = ServiceControl::acquire()? else {
        return Ok(());
    };
    let state =
        crate::server::ServerState::new(config.clone(), crate::tailnet::real(&config.tailnet));
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(crate::server::serve(state.clone(), receiver));
    let _ready = ready_file
        .map(|path| ReadyFile::create(path, ready_token.as_deref()))
        .transpose()?;
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = interval.tick() => { if control.take_stop_request()? { break; } }
        }
    }
    shutdown.send_replace(true);
    state.stop().await;
    server.await??;
    Ok(())
}

struct ReadyFile {
    path: PathBuf,
}

impl ReadyFile {
    fn create(path: PathBuf, token: Option<&str>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("cannot create readiness directory {}", parent.display())
            })?;
        }
        if token.is_some_and(|value| value.contains('\r') || value.contains('\n')) {
            anyhow::bail!("readiness token cannot contain a newline");
        }
        let token_line = token.map_or_else(String::new, |value| format!("token={value}\n"));
        std::fs::write(
            &path,
            format!(
                "pid={}\nversion={}\n{}",
                std::process::id(),
                env!("CARGO_PKG_VERSION"),
                token_line
            ),
        )
        .with_context(|| format!("cannot write readiness marker {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for ReadyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(feature = "media")]
pub(crate) fn spawn_media(
    peer: &crate::rtc::HostPeer,
    config: &AgentConfig,
) -> crate::media::VideoPipeline {
    crate::media::spawn_video(
        peer.video_track(),
        peer.keyframe_requests(),
        peer.link_feedback(),
        peer.display_selection(),
        peer.display_selection_feedback_sender(),
        crate::media::VideoQuality {
            max_bitrate_kbps: config.quality.max_bitrate_kbps,
            max_fps: config.quality.max_fps,
            max_width: config.quality.max_width,
            max_height: config.quality.max_height,
            latency_mode: config.quality.latency_mode,
            encoder_preference: config.quality.encoder,
        },
    )
}

#[cfg(test)]
mod readiness_tests {
    use super::ReadyFile;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn readiness_marker_is_transaction_bound_and_removed() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nanoctl-ready-{}-{unique}.marker",
            std::process::id()
        ));
        let marker = ReadyFile::create(path.clone(), Some("transaction-123"))
            .expect("readiness marker should be writable");
        let contents = std::fs::read_to_string(&path).expect("readiness marker should be readable");
        assert!(contents.contains("pid="));
        assert!(contents.contains("version="));
        assert!(contents.contains("token=transaction-123"));
        drop(marker);
        assert!(!path.exists());
    }
}
