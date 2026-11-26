mod chunking;
mod latest;
mod pipeline;
mod types;

pub use chunking::{ChunkingLaneWriter, VideoChunkSink, VideoLane};
pub use latest::{LatestSlot, PutOutcome};
pub use pipeline::{MediaPipeline, MediaPipelineReport};
pub use types::{
    EncodeRequest, EncodedAccessUnit, EncodedBitstream, LaneWriteReport, LaneWriteStatus,
    LaneWriter, MediaError, MediaPipelineConfig, MediaPipelineSnapshot, PixelFormat, RawFrame,
    VideoEncoder, VideoSource,
};

/// Capture-to-encode handoff capacity. Video intentionally has one hot spare at most:
/// if encode or send falls behind, newer capture replaces the spare rather than queueing
/// latency behind old frames.
pub const LATEST_FRAME_CAPACITY: usize = 1;
