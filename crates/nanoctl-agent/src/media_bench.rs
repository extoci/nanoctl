use std::collections::HashSet;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::time::timeout;
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

use crate::config::{EncoderPreference, LatencyMode};
use crate::media::{VideoPipeline, VideoQuality};
use crate::rtc::HostPeer;

#[derive(Default)]
struct Received {
    start: Option<Instant>,
    bytes: Vec<u64>,
    frames: u64,
    timestamps: HashSet<u32>,
    arrivals_ms: Vec<f64>,
}

impl Received {
    fn marker(&mut self, timestamp: u32, elapsed_ms: f64) {
        if self.timestamps.insert(timestamp) {
            self.frames += 1;
            self.arrivals_ms.push(elapsed_ms);
        }
    }
}

fn percentile(sorted: &[f64], fraction: f64) -> Option<f64> {
    sorted
        .get(((sorted.len() as f64 * fraction).ceil() as usize).saturating_sub(1))
        .copied()
}

fn cpu_ticks() -> Result<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let (_, fields) = stat.rsplit_once(')').context("malformed process stat")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    Ok(fields.get(11).context("missing utime")?.parse::<u64>()?
        + fields.get(12).context("missing stime")?.parse::<u64>()?)
}

fn pipeline_stats(pipeline: &VideoPipeline) -> Value {
    let stats = &pipeline.stats;
    json!({
        "frames_captured": stats.frames_captured.load(Ordering::Relaxed),
        "frames_encoded": stats.frames_encoded.load(Ordering::Relaxed),
        "skipped_unchanged": stats.skipped_unchanged.load(Ordering::Relaxed),
        "keepalives": stats.keepalives.load(Ordering::Relaxed),
        "bytes_sent": stats.bytes_sent.load(Ordering::Relaxed),
        "target_kbps": stats.target_kbps.load(Ordering::Relaxed),
        "target_fps": stats.target_fps.load(Ordering::Relaxed),
    })
}

fn feedback_stats(host: &HostPeer) -> Value {
    let feedback = host.link_feedback();
    feedback.borrow().map_or(Value::Null, |sample| {
        json!({
            "loss": sample.fraction_lost,
            "rtt_ms": sample.rtt.map(|rtt| rtt.as_secs_f64() * 1000.0),
            "remb_kbps": sample.remb_kbps,
        })
    })
}

pub async fn run(seconds: u64) -> Result<Value> {
    ensure!(
        crate::server::dev_user().is_some(),
        "set NANOCTL_DEV_LOOPBACK_USER=bench to enable host loopback ICE"
    );
    let ticks_per_second: f64 = String::from_utf8(
        std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()?
            .stdout,
    )?
    .trim()
    .parse()?;
    let received = Arc::new(Mutex::new(Received::default()));
    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    settings.set_ip_filter(Box::new(|ip| ip.is_loopback()));
    let controller = APIBuilder::new()
        .with_setting_engine(settings)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build()
        .new_peer_connection(RTCConfiguration::default())
        .await?;
    controller.on_track({
        let received = received.clone();
        Box::new(move |track, _, _| {
            let received = received.clone();
            Box::pin(async move {
                while let Ok((packet, _)) = track.read_rtp().await {
                    let now = Instant::now();
                    let mut received = received.lock().unwrap();
                    let Some(start) = received.start else {
                        continue;
                    };
                    let elapsed = now.duration_since(start);
                    let index = elapsed.as_secs() as usize;
                    if index >= received.bytes.len() {
                        continue;
                    }
                    received.bytes[index] += packet.payload.len() as u64;
                    if packet.header.marker {
                        received.marker(packet.header.timestamp, elapsed.as_secs_f64() * 1000.0);
                    }
                }
            })
        })
    });
    controller
        .add_transceiver_from_kind(RTPCodecType::Video, None)
        .await?;
    let _control = controller
        .create_data_channel("nanoctl.control.v1", None)
        .await?;
    let offer = controller.create_offer(None).await?;
    let mut gathering = controller.gathering_complete_promise().await;
    controller.set_local_description(offer).await?;
    let _ = timeout(Duration::from_secs(10), gathering.recv()).await?;
    let offer = controller
        .local_description()
        .await
        .context("missing offer")?;
    let serialized = json!({
        "version": 2, "sessionId": "media-bench", "controllerEpoch": "bench-controller",
        "hostEpoch": "", "sequence": 0, "sender": "controller", "sentAt": 1,
        "payload": {"type": "offer", "sdp": offer.sdp}
    })
    .to_string();
    let (signals_tx, mut signals_rx) = tokio::sync::mpsc::channel(256);
    let host = HostPeer::answer_with_host_epoch(
        signals_tx,
        "media-bench".into(),
        &serialized,
        Vec::new(),
        RTCIceTransportPolicy::All,
        "bench-host".into(),
        false,
    )
    .await?;
    timeout(Duration::from_secs(20), async {
        while controller.connection_state() != RTCPeerConnectionState::Connected {
            if let Ok(Some(signal)) = timeout(Duration::from_millis(100), signals_rx.recv()).await {
                let payload = &signal["envelope"]["payload"];
                match payload["type"].as_str() {
                    Some("answer") => {
                        controller
                            .set_remote_description(RTCSessionDescription::answer(
                                payload["sdp"]
                                    .as_str()
                                    .context("missing answer SDP")?
                                    .into(),
                            )?)
                            .await?
                    }
                    Some("ice-candidate") => {
                        controller
                            .add_ice_candidate(RTCIceCandidateInit {
                                candidate: payload["candidate"]
                                    .as_str()
                                    .context("missing candidate")?
                                    .into(),
                                sdp_mid: payload["sdpMid"].as_str().map(str::to_owned),
                                sdp_mline_index: payload["sdpMLineIndex"]
                                    .as_u64()
                                    .map(|v| v as u16),
                                username_fragment: None,
                            })
                            .await?
                    }
                    _ => {}
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("loopback ICE connection timed out")??;
    let pipeline = crate::media::spawn_video(
        host.video_track(),
        host.keyframe_requests(),
        host.link_feedback(),
        host.display_selection(),
        host.display_selection_feedback_sender(),
        VideoQuality {
            max_bitrate_kbps: 8000,
            max_fps: 60,
            max_width: 1920,
            max_height: 1080,
            latency_mode: LatencyMode::Balanced,
            encoder_preference: EncoderPreference::Software,
        },
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    let initial = pipeline_stats(&pipeline);
    let start_ticks = cpu_ticks()?;
    let start = Instant::now();
    *received.lock().unwrap() = Received {
        start: Some(start),
        bytes: vec![0; seconds as usize],
        ..Default::default()
    };
    let mut target_series = Vec::new();
    for second in 1..=seconds {
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            start + Duration::from_secs(second),
        ))
        .await;
        let mut snapshot = pipeline_stats(&pipeline);
        snapshot["feedback"] = feedback_stats(&host);
        target_series.push(snapshot);
    }
    let end_ticks = cpu_ticks()?;
    let elapsed = start.elapsed().as_secs_f64();
    let mut received = {
        let mut received = received.lock().unwrap();
        received.start = None;
        std::mem::take(&mut *received)
    };
    let final_stats = pipeline_stats(&pipeline);
    let finished_early = pipeline.is_finished();
    pipeline.stop().await;
    host.close().await?;
    controller.close().await?;
    ensure!(
        !finished_early,
        "video pipeline terminated during benchmark"
    );
    ensure!(received.frames > 0, "no video received during measurement");
    let mut intervals: Vec<_> = received
        .arrivals_ms
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    intervals.sort_by(f64::total_cmp);
    let bytes: u64 = received.bytes.iter().sum();
    let mut delta = final_stats.clone();
    for key in [
        "frames_captured",
        "frames_encoded",
        "skipped_unchanged",
        "keepalives",
        "bytes_sent",
    ] {
        delta[key] = match (final_stats[key].as_u64(), initial[key].as_u64()) {
            (Some(end), Some(start)) => json!(end.saturating_sub(start)),
            _ => Value::Null,
        };
    }
    Ok(json!({
        "schema_version": 1, "seconds": elapsed, "warmup_seconds": 5,
        "quality": {"max_width": 1920, "max_height": 1080, "ceiling_kbps": 8000, "max_fps": 60, "encoder": "OpenH264"},
        "process_cpu_percent": (end_ticks - start_ticks) as f64 / ticks_per_second / elapsed * 100.0,
        "received_payload_kbps_avg": bytes as f64 * 8.0 / elapsed / 1000.0,
        "received_payload_kbps_per_second": received.bytes.drain(..).map(|bytes| bytes as f64 * 8.0 / 1000.0).collect::<Vec<_>>(),
        "frames_received": received.frames,
        "inter_frame_ms": {"p50": percentile(&intervals, 0.50), "p95": percentile(&intervals, 0.95),
            "p99": percentile(&intervals, 0.99), "max": intervals.last()},
        "pipeline": delta, "target_series": target_series,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_handle_empty_singleton_and_nearest_rank() {
        assert_eq!(percentile(&[], 0.99), None);
        assert_eq!(percentile(&[2.0], 0.50), Some(2.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.50), Some(2.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.99), Some(4.0));
    }

    #[test]
    fn reads_process_cpu_ticks() {
        assert!(cpu_ticks().is_ok());
    }

    #[test]
    fn retransmitted_markers_do_not_count_as_new_frames() {
        let mut received = Received::default();
        received.marker(1, 10.0);
        received.marker(2, 20.0);
        received.marker(1, 30.0);
        assert_eq!(received.frames, 2);
        assert_eq!(received.arrivals_ms, [10.0, 20.0]);
    }
}
