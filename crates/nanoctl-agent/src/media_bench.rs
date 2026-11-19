use std::collections::{HashMap, HashSet};
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
    sender_reports: u64,
    matching_sender_reports: u64,
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
    stat_ticks(&stat).map(|(_, ticks)| ticks)
}

fn stat_ticks(stat: &str) -> Result<(String, u64)> {
    let (_, fields) = stat.rsplit_once(')').context("malformed process stat")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let ticks = fields.get(11).context("missing utime")?.parse::<u64>()?
        + fields.get(12).context("missing stime")?.parse::<u64>()?;
    let (prefix, _) = stat.rsplit_once(')').context("malformed process stat")?;
    let name = prefix.split_once('(').context("missing thread name")?.1;
    Ok((name.to_owned(), ticks))
}

fn thread_ticks() -> Result<HashMap<String, (String, u64)>> {
    let mut threads = HashMap::new();
    for entry in std::fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        threads.insert(
            entry.file_name().to_string_lossy().into_owned(),
            stat_ticks(&stat)?,
        );
    }
    Ok(threads)
}

fn pipeline_stats(pipeline: &VideoPipeline) -> Value {
    let stats = &pipeline.stats;
    json!({
        "frames_captured": stats.frames_captured.load(Ordering::Relaxed),
        "damage_enabled": stats.damage_enabled.load(Ordering::Relaxed),
        "capture_parked": stats.capture_parked.load(Ordering::Relaxed),
        "damage_resumes": stats.damage_resumes.load(Ordering::Relaxed),
        "first_change_samples": stats.first_change_samples.load(Ordering::Relaxed),
        "first_change_last_ms": stats.first_change_last_ns.load(Ordering::Relaxed) as f64 / 1e6,
        "first_change_max_ms": stats.first_change_max_ns.load(Ordering::Relaxed) as f64 / 1e6,
        "first_change_capture_ready_ms": stats.first_change_capture_ready_ns.load(Ordering::Relaxed) as f64 / 1e6,
        "first_change_encode_ms": stats.first_change_encode_ns.load(Ordering::Relaxed) as f64 / 1e6,
        "frames_encoded": stats.frames_encoded.load(Ordering::Relaxed),
        "changed_encodes": stats.changed_encodes.load(Ordering::Relaxed),
        "unchanged_encodes": stats.unchanged_encodes.load(Ordering::Relaxed),
        "skipped_unchanged": stats.skipped_unchanged.load(Ordering::Relaxed),
        "keepalives": stats.keepalives.load(Ordering::Relaxed),
        "bytes_sent": stats.bytes_sent.load(Ordering::Relaxed),
        "target_kbps": stats.target_kbps.load(Ordering::Relaxed),
        "target_fps": stats.target_fps.load(Ordering::Relaxed),
        "encoded_width": stats.encoded_width.load(Ordering::Relaxed),
        "encoded_height": stats.encoded_height.load(Ordering::Relaxed),
        "fresh_frames": stats.fresh_frames.load(Ordering::Relaxed),
        "compare_ns": stats.compare_ns.load(Ordering::Relaxed),
        "capture_wait_ns": stats.capture_wait_ns.load(Ordering::Relaxed),
        "preprocess_ns": stats.preprocess_ns.load(Ordering::Relaxed),
        "backend_ns": stats.backend_ns.load(Ordering::Relaxed),
        "color_convert_ns": stats.color_convert_ns.load(Ordering::Relaxed),
        "encode_attempts": stats.encode_attempts.load(Ordering::Relaxed),
        "empty_encodes": stats.empty_encodes.load(Ordering::Relaxed),
        "pacer_skips": stats.pacer_skips.load(Ordering::Relaxed),
        "producer_tid": stats.producer_tid.load(Ordering::Relaxed),
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
        Box::new(move |track, receiver, _| {
            let received = received.clone();
            Box::pin(async move {
                let reports = received.clone();
                let ssrc = track.ssrc();
                tokio::spawn(async move {
                    while let Ok((packets, _)) = receiver.read_rtcp().await {
                        for packet in packets {
                            if let Some(sr) = packet
                                .as_any()
                                .downcast_ref::<webrtc::rtcp::sender_report::SenderReport>(
                            ) {
                                let mut reports = reports.lock().unwrap();
                                reports.sender_reports += 1;
                                if sr.ssrc == ssrc && sr.ntp_time != 0 {
                                    reports.matching_sender_reports += 1;
                                }
                            }
                        }
                    }
                });
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
    let initial_threads = thread_ticks()?;
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
    let mut thread_cpu: Vec<_> = thread_ticks()?.into_iter().map(|(tid, (name, ticks))| {
        let previous = initial_threads.get(&tid).map_or(0, |(_, ticks)| *ticks);
        json!({"tid": tid, "name": name, "cpu_percent": ticks.saturating_sub(previous) as f64 / ticks_per_second / elapsed * 100.0})
    }).collect();
    thread_cpu.sort_by(|a, b| {
        b["cpu_percent"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["cpu_percent"].as_f64().unwrap())
    });
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
        "damage_resumes",
        "first_change_samples",
        "frames_encoded",
        "changed_encodes",
        "unchanged_encodes",
        "skipped_unchanged",
        "keepalives",
        "bytes_sent",
        "fresh_frames",
        "compare_ns",
        "capture_wait_ns",
        "preprocess_ns",
        "backend_ns",
        "color_convert_ns",
        "encode_attempts",
        "empty_encodes",
        "pacer_skips",
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
        "thread_cpu_percent": thread_cpu,
        "received_payload_kbps_avg": bytes as f64 * 8.0 / elapsed / 1000.0,
        "received_payload_kbps_per_second": received.bytes.drain(..).map(|bytes| bytes as f64 * 8.0 / 1000.0).collect::<Vec<_>>(),
        "frames_received": received.frames,
        "sender_reports_received": received.sender_reports,
        "sender_reports_matching_video": received.matching_sender_reports,
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
        assert!(!thread_ticks().unwrap().is_empty());
    }

    #[test]
    fn stat_parser_handles_thread_names_and_invalid_or_empty_records() {
        let stat = "123 (a (worker)) S 0 0 0 0 0 0 0 0 0 0 7 3";
        assert_eq!(stat_ticks(stat).unwrap(), ("a (worker)".to_owned(), 10));
        for invalid in [
            "",
            "123 no parentheses",
            "123 (a) S",
            "123 (a) S 0 0 0 0 0 0 0 0 0 0 x 3",
        ] {
            assert!(stat_ticks(invalid).is_err());
        }
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
