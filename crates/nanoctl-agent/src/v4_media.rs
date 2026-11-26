use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use nanoctl_media::{
    ChunkingLaneWriter, EncoderSelection, MediaPipeline, MediaPipelineConfig, PlatformEncoder,
    VideoChunkSink, VideoLane,
};
use webrtc::data_channel::RTCDataChannel;

use crate::config::AgentConfig;
use crate::rtc::{HostPeer, V4DataChannelVideo};

const DELTA_LANE_CAPACITY: usize = 128 * 1024;
const REFERENCE_LANE_CAPACITY: usize = 512 * 1024;

struct WebRtcVideoSink {
    lanes: Arc<V4DataChannelVideo>,
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
            run_pipeline(lanes, pipeline_config, encoder_selection, shutdown_rx).await;
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

async fn run_pipeline(
    lanes: Arc<V4DataChannelVideo>,
    config: MediaPipelineConfig,
    encoder_selection: EncoderSelection,
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
    let sink = WebRtcVideoSink {
        lanes: lanes.clone(),
    };
    let lane = ChunkingLaneWriter::new(sink);
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
        encoded = report.snapshot.access_units_encoded,
        written = report.snapshot.lane_writes,
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
