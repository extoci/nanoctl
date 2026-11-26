mod chunking;
mod latest;
mod nv12;
mod pipeline;
mod types;
#[cfg(feature = "capture")]
mod xcap_capture;

pub use chunking::{ChunkingLaneWriter, VideoChunkSink, VideoLane};
pub use latest::{LatestSlot, PutOutcome};
pub use nv12::frame_to_nv12;
pub use pipeline::{MediaPipeline, MediaPipelineReport};
pub use types::{
    EncodeRequest, EncodedAccessUnit, EncodedBitstream, LaneWriteReport, LaneWriteStatus,
    LaneWriter, MediaError, MediaPipelineConfig, MediaPipelineSnapshot, PixelFormat, RawFrame,
    VideoEncoder, VideoSource,
};
#[cfg(feature = "capture")]
pub use xcap_capture::XcapCaptureSource;

/// Capture-to-encode handoff capacity. Video intentionally has one hot spare at most:
/// if encode or send falls behind, newer capture replaces the spare rather than queueing
/// latency behind old frames.
pub const LATEST_FRAME_CAPACITY: usize = 1;
