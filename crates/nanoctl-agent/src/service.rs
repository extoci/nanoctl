use crate::{config::AgentConfig, service_control::ServiceControl};
use anyhow::{Context, Result};
use std::{future::Future, path::PathBuf, time::Duration};

pub async fn run(
    config: AgentConfig,
    control: ServiceControl,
    ready_file: Option<PathBuf>,
    ready_token: Option<String>,
) -> Result<()> {
    run_until(config, ready_file, ready_token, async {
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("cannot listen for Ctrl+C").map(|()| "Ctrl+C"),
            result = wait_for_stop_request(&control) => result,
        }
    })
    .await
}

pub(crate) async fn wait_for_stop_request(control: &ServiceControl) -> Result<&'static str> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        if control.take_stop_request()? {
            return Ok("stop request");
        }
    }
}

#[cfg(windows)]
pub(crate) async fn run_worker(config: AgentConfig) -> Result<()> {
    let mut interrupt = tokio::signal::windows::ctrl_c().context("cannot listen for Ctrl+C")?;
    let parent = crate::supervisor::parent_shutdown()?;
    run_until(config, None, None, async {
        tokio::select! {
            _ = interrupt.recv() => Ok("Ctrl+C"),
            result = parent => result.context("supervisor shutdown pipe stopped").map(|()| "supervisor"),
        }
    })
    .await
}

#[cfg(all(feature = "media", windows))]
struct TimerResolutionGuard(bool);

#[cfg(all(feature = "media", windows))]
impl TimerResolutionGuard {
    /// Windows otherwise quantizes ordinary timer waits to a much coarser
    /// scheduler tick, which shows up directly as capture/pacing jitter at
    /// 60fps and above. The process-wide request is paired on shutdown and
    /// released by Windows even if the agent exits unexpectedly.
    fn acquire() -> Self {
        let acquired = unsafe { windows::Win32::Media::timeBeginPeriod(1) } == 0;
        if !acquired {
            tracing::warn!("Windows high-resolution timer request failed");
        }
        Self(acquired)
    }
}

#[cfg(all(feature = "media", windows))]
impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::Media::timeEndPeriod(1) };
        }
    }
}

async fn run_until(
    config: AgentConfig,
    ready_file: Option<PathBuf>,
    ready_token: Option<String>,
    shutdown_requested: impl Future<Output = Result<&'static str>>,
) -> Result<()> {
    #[cfg(all(feature = "media", windows))]
    let _timer_resolution = TimerResolutionGuard::acquire();
    let state =
        crate::server::ServerState::new(config.clone(), crate::tailnet::real(&config.tailnet));
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(crate::server::serve(state.clone(), receiver));
    #[cfg(feature = "media")]
    {
        // Warm the stream replay window at boot so even a first-ever attach
        // replays a tail instead of waiting for a pipeline cold start.
        let (bitrate, fps, width, height, mode, encoder) = (
            config.quality.max_bitrate_kbps,
            crate::media::effective_max_fps(config.quality.max_fps),
            config.quality.max_width,
            config.quality.max_height,
            config.quality.latency_mode,
            config.quality.encoder,
        );
        tokio::task::spawn_blocking(move || {
            if let Err(error) =
                crate::media::prime_replay(bitrate, fps, width, height, mode, encoder)
            {
                tracing::debug!(%error, "video replay warmup failed");
            }
        });
    }
    let _ready = ready_file
        .map(|path| ReadyFile::create(path, ready_token.as_deref()))
        .transpose()?;
    let result = shutdown_requested.await;
    if let Ok(reason) = &result {
        tracing::info!(target: "nanoctl::status", reason, "Agent shutting down");
    }
    shutdown.send_replace(true);
    state.stop().await;
    let server_result = server.await;
    result?;
    server_result??;
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
        peer.video_data_channel(),
        peer.keyframe_requests(),
        peer.link_feedback(),
        peer.viewer_limits(),
        peer.display_selection(),
        peer.display_selection_feedback_sender(),
        crate::media::VideoQuality {
            max_bitrate_kbps: config.quality.max_bitrate_kbps,
            max_fps: crate::media::effective_max_fps(config.quality.max_fps),
            max_width: config.quality.max_width,
            max_height: config.quality.max_height,
            latency_mode: config.quality.latency_mode,
            encoder_preference: config.quality.encoder,
            codec: peer.video_codec(),
            h264_high: peer.h264_high(),
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
