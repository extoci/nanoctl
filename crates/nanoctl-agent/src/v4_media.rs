use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use nanoctl_media::{
    ChunkingLaneWriter, EncoderSelection, MediaPipeline, MediaPipelineConfig, MediaQuality,
    PlatformEncoder, VideoChunkSink, VideoLane,
};
use webrtc::data_channel::RTCDataChannel;

use crate::config::AgentConfig;
use crate::rtc::{HostPeer, V4DataChannelVideo};

const MIN_DELTA_LANE_CAPACITY: usize = 64 * 1024;
const MAX_DELTA_LANE_CAPACITY: usize = 512 * 1024;
const MIN_REFERENCE_LANE_CAPACITY: usize = 192 * 1024;
const MAX_REFERENCE_LANE_CAPACITY: usize = 768 * 1024;

struct WebRtcVideoSink {
    lanes: Arc<V4DataChannelVideo>,
    quality: tokio::sync::watch::Receiver<MediaQuality>,
}

impl WebRtcVideoSink {
    fn channel(&self, lane: VideoLane) -> Option<Arc<RTCDataChannel>> {
        match lane {
            VideoLane::Delta => self.lanes.delta_channel.lock().unwrap().clone(),
            VideoLane::Reference => self.lanes.reference_channel.lock().unwrap().clone(),
        }
    }

    fn capacity(&self, lane: VideoLane) -> usize {
        let quality = self.quality.borrow();
        let bytes_per_frame = usize::try_from(quality.target_bitrate).unwrap_or(usize::MAX)
            / 8
            / usize::try_from(quality.frame_rate).unwrap_or(1).max(1);
        match lane {
            VideoLane::Delta => {
                (bytes_per_frame * 4).clamp(MIN_DELTA_LANE_CAPACITY, MAX_DELTA_LANE_CAPACITY)
            }
            VideoLane::Reference => (bytes_per_frame * 10)
                .clamp(MIN_REFERENCE_LANE_CAPACITY, MAX_REFERENCE_LANE_CAPACITY),
        }
    }
}

#[async_trait]
impl VideoChunkSink for WebRtcVideoSink {
    async fn buffered_bytes(&self, lane: VideoLane) -> usize {
        let Some(channel) = self.channel(lane) else {
            return self.capacity(lane);
        };
        channel.buffered_amount().await
    }

    async fn lane_capacity(&self, lane: VideoLane) -> usize {
        self.capacity(lane)
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
            frame_rate: u32::from(crate::media::effective_max_fps(config.quality.max_fps)).min(60),
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
                shutdown_rx,
                viewer_limits,
                link_feedback,
            )
            .await;
            drop(task_shutdown);
        });
        Self { shutdown, task }
    }

    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        if let Err(error) = self.task.await {
            tracing::warn!(%error, "v4 media task join failed");
        }
    }
}

fn viewer_rate_limits(
    config: MediaPipelineConfig,
    limits: crate::adaptive::ViewerLimits,
) -> (u32, u16) {
    limits.resolve(
        config.target_bitrate / 1000,
        config.frame_rate.min(u32::from(u16::MAX)) as u16,
    )
}

fn quality_for_limits(
    config: MediaPipelineConfig,
    limits: crate::adaptive::ViewerLimits,
    controller: &crate::adaptive::RateController,
) -> MediaQuality {
    let (width, height) = limits.resolve_dimensions(config.width, config.height);
    let (bitrate_ceiling_kbps, fps_ceiling) = viewer_rate_limits(config, limits);
    let decision = controller.decision();
    MediaQuality {
        width,
        height,
        frame_rate: u32::from(decision.fps.min(fps_ceiling)),
        target_bitrate: decision
            .bitrate_kbps
            .min(bitrate_ceiling_kbps)
            .saturating_mul(1000),
    }
    .clamped(config)
}

async fn run_pipeline(
    lanes: Arc<V4DataChannelVideo>,
    config: MediaPipelineConfig,
    encoder_selection: EncoderSelection,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    mut viewer_limits: tokio::sync::watch::Receiver<crate::adaptive::ViewerLimits>,
    mut link_feedback: tokio::sync::watch::Receiver<Option<crate::adaptive::LinkSample>>,
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
    let initial_limits = *viewer_limits.borrow();
    let (initial_bitrate, initial_fps) = viewer_rate_limits(config, initial_limits);
    let mut controller = crate::adaptive::RateController::new(initial_bitrate, initial_fps);
    let initial_quality = quality_for_limits(config, initial_limits, &controller);
    let (quality_tx, quality_rx) = tokio::sync::watch::channel(initial_quality);
    let quality_updates = tokio::spawn(async move {
        let mut limits = initial_limits;
        loop {
            tokio::select! {
                changed = viewer_limits.changed() => {
                    if changed.is_err() { break; }
                    limits = *viewer_limits.borrow();
                    let (bitrate, fps) = viewer_rate_limits(config, limits);
                    controller.set_limits(bitrate, fps);
                }
                changed = link_feedback.changed() => {
                    if changed.is_err() { break; }
                    if let Some(sample) = *link_feedback.borrow() {
                        controller.observe(sample);
                    }
                }
            }
            let quality = quality_for_limits(config, limits, &controller);
            let previous = quality_tx.send_replace(quality);
            if previous != quality {
                tracing::info!(
                    width = quality.width,
                    height = quality.height,
                    fps = quality.frame_rate,
                    bitrate_kbps = quality.target_bitrate / 1000,
                    "v4 media quality updated"
                );
            }
        }
    });
    let sink = WebRtcVideoSink {
        lanes: lanes.clone(),
        quality: quality_rx.clone(),
    };
    let lane = ChunkingLaneWriter::new(sink);
    let pipeline = match MediaPipeline::start_with_quality(
        config,
        source,
        Box::new(encoder),
        Box::new(lane),
        lanes.keyframe_requested.clone(),
        quality_rx,
    ) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            quality_updates.abort();
            tracing::warn!(%error, "v4 media pipeline failed to start");
            return;
        }
    };
    tracing::info!(
        width = initial_quality.width,
        height = initial_quality.height,
        fps = initial_quality.frame_rate,
        bitrate_kbps = initial_quality.target_bitrate / 1000,
        "v4 media pipeline started"
    );
    let _ = shutdown.changed().await;
    let report = pipeline.stop().await;
    quality_updates.abort();
    if let Err(error) = report.capture_result {
        tracing::warn!(%error, "v4 capture stopped with an error");
    }
    if let Err(error) = report.encode_result {
        tracing::warn!(%error, "v4 encode stopped with an error");
    }
    tracing::info!(
        captured = report.snapshot.frames_captured,
        encoded = report.snapshot.access_units_encoded,
        written = report.snapshot.lane_writes,
        dropped = report.snapshot.access_units_dropped_before_write,
        max_buffered = report.snapshot.max_buffered_bytes,
        "v4 media pipeline stopped"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MediaPipelineConfig {
        MediaPipelineConfig {
            width: 1920,
            height: 1080,
            frame_rate: 60,
            target_bitrate: 100_000_000,
            codec: nanoctl_core::video::VideoCodec::H264,
        }
    }

    #[test]
    fn viewer_viewport_and_rate_caps_lower_v4_quality() {
        let limits = crate::adaptive::ViewerLimits {
            max_fps: Some(30),
            max_bitrate_kbps: Some(2_000),
            max_width: Some(1320),
            max_height: Some(742),
        };
        let controller = crate::adaptive::RateController::new(2_000, 30);
        let quality = quality_for_limits(config(), limits, &controller);
        assert_eq!(quality.width, 1320);
        assert_eq!(quality.height, 742);
        assert_eq!(quality.frame_rate, 30);
        assert!(quality.target_bitrate <= 2_000_000);
    }

    #[test]
    fn lane_capacity_shrinks_for_constrained_links_and_scales_for_fast_links() {
        let lanes = Arc::new(V4DataChannelVideo {
            delta_channel: std::sync::Mutex::new(None),
            reference_channel: std::sync::Mutex::new(None),
            keyframe_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ready: tokio::sync::Notify::new(),
        });
        let quality = MediaQuality {
            width: 1320,
            height: 742,
            frame_rate: 60,
            target_bitrate: 2_000_000,
        };
        let (_tx, rx) = tokio::sync::watch::channel(quality);
        let constrained = WebRtcVideoSink {
            lanes: lanes.clone(),
            quality: rx,
        };
        assert_eq!(constrained.capacity(VideoLane::Delta), 64 * 1024);
        assert_eq!(constrained.capacity(VideoLane::Reference), 192 * 1024);

        let quality = MediaQuality {
            target_bitrate: 24_000_000,
            ..quality
        };
        let (_tx, rx) = tokio::sync::watch::channel(quality);
        let fast = WebRtcVideoSink { lanes, quality: rx };
        assert!(fast.capacity(VideoLane::Delta) > constrained.capacity(VideoLane::Delta));
        assert!(fast.capacity(VideoLane::Reference) > constrained.capacity(VideoLane::Reference));
    }
}
