use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use nanoctl_core::video::DeltaRepairRequest;
use nanoctl_media::{
    ChunkingLaneWriter, EncoderSelection, MediaPipeline, MediaPipelineConfig, PlatformEncoder,
    VideoChunkSink, VideoLane,
};
use webrtc::data_channel::RTCDataChannel;

use crate::adaptive::{LinkSample, RateController, ViewerLimits};
use crate::config::AgentConfig;
use crate::rtc::{HostPeer, V4DataChannelVideo};

const DELTA_LANE_CAPACITY: usize = 128 * 1024;
const REFERENCE_LANE_CAPACITY: usize = 512 * 1024;
const MIN_KEYFRAME_PACING_BPS: u32 = 2_400_000;

struct WebRtcVideoSink {
    lanes: Arc<V4DataChannelVideo>,
    send_rate_kbps: Arc<AtomicU32>,
}

impl WebRtcVideoSink {
    fn channel(&self, lane: VideoLane) -> Option<Arc<RTCDataChannel>> {
        match lane {
            VideoLane::Delta => self.lanes.delta_channel.lock().unwrap().clone(),
            VideoLane::Reference => self.lanes.reference_channel.lock().unwrap().clone(),
        }
    }

    fn capacity(lane: VideoLane) -> usize {
        match lane {
            VideoLane::Delta => DELTA_LANE_CAPACITY,
            VideoLane::Reference => REFERENCE_LANE_CAPACITY,
        }
    }
}

#[async_trait]
impl VideoChunkSink for WebRtcVideoSink {
    fn take_delta_repair_request(&mut self) -> Option<DeltaRepairRequest> {
        self.lanes.pending_delta_repair.lock().unwrap().take()
    }

    fn keyframe_pacing_bps(&self) -> Option<NonZeroU32> {
        let bps = self
            .send_rate_kbps
            .load(Ordering::Relaxed)
            .saturating_mul(1_000);
        NonZeroU32::new(bps.max(MIN_KEYFRAME_PACING_BPS))
    }

    async fn buffered_bytes(&self, lane: VideoLane) -> usize {
        let Some(channel) = self.channel(lane) else {
            return Self::capacity(lane);
        };
        channel.buffered_amount().await
    }

    async fn lane_capacity(&self, lane: VideoLane) -> usize {
        Self::capacity(lane)
    }

    async fn write_chunk(
        &mut self,
        lane: VideoLane,
        bytes: Vec<u8>,
    ) -> Result<(), nanoctl_media::MediaError> {
        let Some(channel) = self.channel(lane) else {
            return Err(nanoctl_media::MediaError::Lane(
                "v4 video data channel is not open".into(),
            ));
        };
        channel
            .send(&Bytes::from(bytes))
            .await
            .map(|_| ())
            .map_err(|error| nanoctl_media::MediaError::Lane(error.to_string()))
    }
}

pub struct V4MediaRuntime {
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl V4MediaRuntime {
    pub fn spawn(peer: &HostPeer, config: &AgentConfig) -> Self {
        let lanes = peer.v4_video_data_channel();
        let viewer_limits = peer.viewer_limits();
        let link_feedback = peer.link_feedback();
        let pipeline_config = MediaPipelineConfig {
            width: config.quality.max_width,
            height: config.quality.max_height,
            frame_rate: u32::from(crate::media::effective_max_fps(config.quality.max_fps)),
            target_bitrate: config.quality.max_bitrate_kbps.saturating_mul(1000),
            codec: nanoctl_core::video::VideoCodec::H264,
        };
        let encoder_selection = match config.quality.encoder {
            crate::config::EncoderPreference::Auto => EncoderSelection::Auto,
            crate::config::EncoderPreference::Hardware => EncoderSelection::Hardware,
            crate::config::EncoderPreference::Software => EncoderSelection::Software,
        };
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            run_pipeline(
                lanes,
                pipeline_config,
                encoder_selection,
                viewer_limits,
                link_feedback,
                shutdown_rx,
            )
            .await;
            drop(task_shutdown);
        });
        Self { shutdown, task }
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        if let Err(error) = self.task.await {
            tracing::warn!(%error, "v4 media task join failed");
        }
    }
}

async fn run_pipeline(
    lanes: Arc<V4DataChannelVideo>,
    configured: MediaPipelineConfig,
    encoder_selection: EncoderSelection,
    mut viewer_limits: tokio::sync::watch::Receiver<ViewerLimits>,
    mut link_feedback: tokio::sync::watch::Receiver<Option<LinkSample>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    while !lanes.lanes_ready() {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() { return; }
            }
            _ = lanes.ready.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    if *shutdown.borrow() {
        return;
    }

    #[cfg(target_os = "windows")]
    let source: Box<dyn nanoctl_media::VideoSource> =
        match nanoctl_media::DxgiCaptureSource::primary() {
            Ok(source) => {
                tracing::info!("v4 media using DXGI capture");
                Box::new(source)
            }
            Err(error) => {
                tracing::warn!(%error, "DXGI capture unavailable; falling back to xcap");
                match nanoctl_media::XcapCaptureSource::primary() {
                    Ok(source) => Box::new(source),
                    Err(error) => {
                        tracing::warn!(%error, "v4 capture source failed to start");
                        return;
                    }
                }
            }
        };
    #[cfg(not(target_os = "windows"))]
    let source: Box<dyn nanoctl_media::VideoSource> =
        match nanoctl_media::XcapCaptureSource::primary() {
            Ok(source) => Box::new(source),
            Err(error) => {
                tracing::warn!(%error, "v4 capture source failed to start");
                return;
            }
        };

    let mut limits = *viewer_limits.borrow_and_update();
    let ceiling_kbps = (configured.target_bitrate / 1_000).max(1);
    let max_fps = configured.frame_rate as u16;
    let (ceiling, fps) = limits.resolve(ceiling_kbps, max_fps);
    let mut rate = RateController::with_opening_limit(ceiling, fps, 1_800);
    let config = media_settings(configured, limits, &rate);
    let encoder = match PlatformEncoder::new(
        encoder_selection,
        config.target_bitrate,
        config.frame_rate,
        config.codec,
    ) {
        Ok(encoder) => encoder,
        Err(error) => {
            tracing::warn!(%error, "v4 encoder failed to start");
            return;
        }
    };
    let send_rate_kbps = Arc::new(AtomicU32::new(rate.decision().bitrate_kbps));
    let sink = WebRtcVideoSink {
        lanes: lanes.clone(),
        send_rate_kbps: send_rate_kbps.clone(),
    };
    let lane = ChunkingLaneWriter::with_repeated_chunks(sink);
    let pipeline = match MediaPipeline::start_with_keyframe_requests(
        config,
        source,
        Box::new(encoder),
        Box::new(lane),
        lanes.keyframe_requested.clone(),
    ) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            tracing::warn!(%error, "v4 media pipeline failed to start");
            return;
        }
    };
    tracing::info!("v4 media pipeline started");
    let mut idle_tick = tokio::time::interval(Duration::from_millis(100));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous_sequence = None;
    let mut last_watermark = None;
    let mut feedback_tick = tokio::time::interval(Duration::from_millis(250));
    feedback_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous = pipeline.snapshot();
    let mut last_sample = Instant::now();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = idle_tick.tick() => {
                let next = pipeline.snapshot().next_video_sequence;
                if let Some(sequence) = next
                    && previous_sequence == next
                    && last_watermark != next
                    && send_watermark(&lanes, sequence).await
                {
                    last_watermark = next;
                }
                previous_sequence = next;
            }
            _ = feedback_tick.tick() => {
                if pipeline.is_finished() {
                    break;
                }
                if viewer_limits.has_changed().unwrap_or(false) {
                    limits = *viewer_limits.borrow_and_update();
                    let (ceiling, fps) = limits.resolve(ceiling_kbps, max_fps);
                    rate.set_limits(ceiling, fps);
                }
                let now = Instant::now();
                let snapshot = pipeline.snapshot();
                let elapsed = now.saturating_duration_since(last_sample).as_secs_f64();
                let bytes = snapshot.bytes_written.saturating_sub(previous.bytes_written);
                let backpressured = snapshot.access_units_dropped_before_write
                    > previous.access_units_dropped_before_write
                    || snapshot.frames_skipped_for_backpressure > previous.frames_skipped_for_backpressure;
                if elapsed >= 0.05 {
                    rate.update_sender(
                        bytes as f64 * 8.0 / (1_000.0 * elapsed),
                        bytes == 0 && !backpressured,
                        now,
                    );
                }
                if backpressured {
                    rate.backpressure(now);
                }
                if link_feedback.has_changed().unwrap_or(false)
                    && let Some(mut feedback) = *link_feedback.borrow_and_update()
                {
                    // RTP bitrate estimates describe a different media path.
                    feedback.remb_kbps = None;
                    rate.observe(feedback);
                }
                if let Err(error) = pipeline.update_settings(media_settings(configured, limits, &rate)) {
                    tracing::warn!(%error, "could not update v4 media settings");
                    break;
                }
                send_rate_kbps.store(rate.decision().bitrate_kbps, Ordering::Relaxed);
                previous = snapshot;
                last_sample = now;
            }
        }
    }
    let report = pipeline.stop().await;
    if let Err(error) = report.capture_result {
        tracing::warn!(%error, "v4 capture stopped with an error");
    }
    if let Err(error) = report.encode_result {
        tracing::warn!(%error, "v4 encode stopped with an error");
    }
    tracing::info!(
        captured = report.snapshot.frames_captured,
        taken = report.snapshot.frames_taken_for_encode,
        waiting_for_reference = report.snapshot.frames_waiting_for_reference,
        skipped_for_backpressure = report.snapshot.frames_skipped_for_backpressure,
        encoded = report.snapshot.access_units_encoded,
        encoder_skips = report.snapshot.encoder_skips,
        keyframes = report.snapshot.keyframes_encoded,
        deltas_during_keyframe_cooldown = report.snapshot.deltas_written_during_keyframe_cooldown,
        delta_repairs = report.snapshot.delta_repairs_written,
        max_encode_us = report.snapshot.max_encode_micros,
        written = report.snapshot.lane_writes,
        wire_bytes = report.snapshot.bytes_written,
        dropped = report.snapshot.access_units_dropped_before_write,
        max_buffered = report.snapshot.max_buffered_bytes,
        "v4 media pipeline stopped"
    );
}

async fn send_watermark(lanes: &V4DataChannelVideo, next_sequence: u32) -> bool {
    let channel = lanes.reference_channel.lock().unwrap().clone();
    let Some(channel) = channel else { return false };
    let message = Bytes::from(
        nanoctl_core::video::VideoControlMessage::SequenceWatermark { next_sequence }.encode(),
    );
    matches!(
        tokio::time::timeout(Duration::from_millis(75), async {
            if channel.buffered_amount().await + message.len() > REFERENCE_LANE_CAPACITY {
                return false;
            }
            channel.send(&message).await.is_ok()
        })
        .await,
        Ok(true)
    )
}

fn media_settings(
    config: MediaPipelineConfig,
    limits: ViewerLimits,
    rate: &RateController,
) -> MediaPipelineConfig {
    let (width, height) = limits.resolve_dimensions(config.width, config.height);
    let pixels = f64::from(width) * f64::from(height);
    let base_pixels = f64::from(config.width) * f64::from(config.height);
    let decision = rate.decision();
    let (kbps, _) = crate::media::viewport_scaled_rate(
        decision.bitrate_kbps,
        limits
            .resolve(config.target_bitrate / 1_000, config.frame_rate as u16)
            .0,
        pixels / base_pixels,
    );
    // Reserve room for the second copies on the delta lane.
    let kbps = kbps.div_ceil(2);
    // A constrained link needs enough bits per pixel to encode useful deltas
    // between recovery references. Keep the connection budget when scaling down.
    let (width, height) = if kbps < 1_500 {
        (width.min(640), height.min(360))
    } else if kbps < 2_250 {
        (width.min(960), height.min(540))
    } else {
        (width, height)
    };
    MediaPipelineConfig {
        width,
        height,
        target_bitrate: kbps.saturating_mul(1_000),
        frame_rate: u32::from(decision.fps),
        ..config
    }
}
