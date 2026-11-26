mod bitstream;
mod chunking;
#[cfg(all(windows, feature = "capture"))]
mod dxgi_capture;
mod i420;
mod latest;
mod nv12;
mod openh264_encoder;
mod pipeline;
mod platform_encoder;
mod scaling;
mod types;
#[cfg(target_os = "windows")]
mod windows_mft;
#[cfg(feature = "capture")]
mod xcap_capture;

pub use chunking::{ChunkingLaneWriter, VideoChunkSink, VideoLane};
#[cfg(all(windows, feature = "capture"))]
pub use dxgi_capture::DxgiCaptureSource;
pub use i420::frame_to_i420;
pub use latest::{LatestSlot, PutOutcome};
pub use nv12::{frame_to_nv12, frame_to_nv12_slice};
pub use openh264_encoder::OpenH264Encoder;
pub use pipeline::{MediaPipeline, MediaPipelineReport};
pub use platform_encoder::{EncoderSelection, PlatformEncoder};
pub use types::{
    EncodeRequest, EncodedAccessUnit, EncodedBitstream, LaneWriteReport, LaneWriteStatus,
    LaneWriter, MediaError, MediaPipelineConfig, MediaPipelineSnapshot, PixelFormat, RawFrame,
    VideoEncoder, VideoSource,
};
#[cfg(target_os = "windows")]
pub use windows_mft::WindowsEncoder;
#[cfg(feature = "capture")]
pub use xcap_capture::XcapCaptureSource;

/// Capture-to-encode handoff capacity. Video intentionally has one hot spare at most:
/// if encode or send falls behind, newer capture replaces the spare rather than queueing
/// latency behind old frames.
pub const LATEST_FRAME_CAPACITY: usize = 1;
