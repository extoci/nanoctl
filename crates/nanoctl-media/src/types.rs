use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nanoctl_core::video::VideoCodec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    InvalidConfig(&'static str),
    Capture(String),
    Encode(String),
    EncoderContract(&'static str),
    InvalidAccessUnit(&'static str),
    Lane(String),
    TaskJoin(String),
}

impl Display for MediaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(formatter, "invalid media config: {reason}"),
            Self::Capture(reason) => write!(formatter, "capture failed: {reason}"),
            Self::Encode(reason) => write!(formatter, "encode failed: {reason}"),
            Self::EncoderContract(reason) => {
                write!(formatter, "encoder contract violation: {reason}")
            }
            Self::InvalidAccessUnit(reason) => {
                write!(formatter, "invalid encoded access unit: {reason}")
            }
            Self::Lane(reason) => write!(formatter, "media lane failed: {reason}"),
            Self::TaskJoin(reason) => write!(formatter, "media worker join failed: {reason}"),
        }
    }
}

impl Error for MediaError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaPipelineConfig {
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub target_bitrate: u32,
    pub codec: VideoCodec,
}

impl MediaPipelineConfig {
    pub const MAX_FRAME_RATE: u32 = 240;

    pub fn validate(self) -> Result<Self, MediaError> {
        if self.width == 0 {
            return Err(MediaError::InvalidConfig("width must be nonzero"));
        }
        if self.height == 0 {
            return Err(MediaError::InvalidConfig("height must be nonzero"));
        }
        if self.frame_rate == 0 || self.frame_rate > Self::MAX_FRAME_RATE {
            return Err(MediaError::InvalidConfig(
                "frame rate must be between 1 and 240",
            ));
        }
        if self.target_bitrate == 0 {
            return Err(MediaError::InvalidConfig("target bitrate must be nonzero"));
        }
        Ok(self)
    }

    pub fn frame_interval(self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.frame_rate))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Nv12,
    I420,
    Rgba32,
    Bgra32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Host-local monotonic capture timestamp. Send or encode completion time is never
    /// substituted for this value because doing so hides queueing delay from measurements.
    pub capture_time_us: u64,
    pub capture_sequence: u64,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Whether the pixels contain new desktop damage since the previously captured frame.
    /// Backends without damage detection leave this true; DXGI can reuse shared pixels with it
    /// false so the encoder does not burn time on unchanged screens unless a keyframe is due.
    pub has_new_damage: bool,
    pub pixels: Arc<[u8]>,
}

impl RawFrame {
    pub fn new(
        capture_time_us: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            capture_time_us,
            capture_sequence: 0,
            width,
            height,
            format,
            has_new_damage: true,
            pixels: pixels.into(),
        }
    }

    /// Creates a frame backed by shared immutable pixels so repeated unchanged desktop frames do
    /// not require another full-screen memcpy before encode.
    pub fn from_shared(
        capture_time_us: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Arc<[u8]>,
    ) -> Self {
        Self {
            capture_time_us,
            capture_sequence: 0,
            width,
            height,
            format,
            has_new_damage: true,
            pixels,
        }
    }

    pub fn unchanged_shared(
        capture_time_us: u64,
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Arc<[u8]>,
    ) -> Self {
        Self {
            capture_time_us,
            capture_sequence: 0,
            width,
            height,
            format,
            has_new_damage: false,
            pixels,
        }
    }
}

#[derive(Debug)]
pub struct EncodeRequest {
    pub stream_sequence: u32,
    pub force_keyframe: bool,
    pub target_bitrate: u32,
    pub codec: VideoCodec,
    pub frame: RawFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBitstream {
    pub keyframe: bool,
    /// Empty means the encoder skipped this image and kept its previous reference state.
    /// The pipeline counts the skip and retries any pending request for a keyframe.
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAccessUnit {
    pub stream_sequence: u32,
    pub keyframe: bool,
    pub codec: VideoCodec,
    pub capture_time_us: u64,
    pub capture_sequence: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneWriteStatus {
    /// Every byte of the access unit was accepted by the lane.
    Written,
    /// No byte of this access unit was emitted. The pipeline will encode the next frame
    /// as a keyframe so dropping it cannot silently corrupt dependent deltas.
    DroppedBeforeWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneWriteReport {
    pub status: LaneWriteStatus,
    pub buffered_bytes: usize,
    /// Video bytes accepted by the transport, including headers and repeated chunks.
    pub wire_bytes: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaPipelineSnapshot {
    pub capture_ticks: u64,
    pub frames_captured: u64,
    pub frames_replaced_before_encode: u64,
    pub frames_taken_for_encode: u64,
    pub frames_waiting_for_reference: u64,
    pub frames_skipped_for_backpressure: u64,
    pub access_units_encoded: u64,
    pub keyframes_encoded: u64,
    pub bytes_encoded: u64,
    pub capture_errors: u64,
    pub encode_errors: u64,
    pub lane_writes: u64,
    pub lane_write_errors: u64,
    pub access_units_dropped_before_write: u64,
    pub encoder_skips: u64,
    pub unchanged_frames_skipped: u64,
    pub bytes_written: u64,
    pub max_buffered_bytes: u64,
    pub next_video_sequence: Option<u32>,
    pub total_encode_micros: u64,
    pub max_encode_micros: u64,
}

#[async_trait]
pub trait VideoSource: Send {
    /// Capture the next frame. Returning `Ok(None)` means the source ended cleanly.
    /// Implementations must use a monotonic capture clock and must not block forever
    /// without honoring task cancellation at their own layer.
    async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError>;
}

#[async_trait]
pub trait VideoEncoder: Send {
    /// Encode one owned request into one access unit. Ownership lets platform backends hand
    /// the frame to a dedicated blocking-thread owner without copying or unsafe references.
    /// The encoder must honor `force_keyframe`; the pipeline treats a non-keyframe response as
    /// a contract violation because dependent deltas would otherwise be emitted unsafely.
    async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError>;
}

#[async_trait]
pub trait LaneWriter: Send {
    /// Whether the transport is ready for a new reference access unit.
    async fn reference_ready(&mut self) -> bool {
        true
    }

    /// Whether the transport has room to begin encoding another delta frame.
    async fn delta_ready(&mut self) -> bool {
        true
    }

    /// Atomically accept or reject one complete access unit. A lane that cannot accept
    /// it without queueing must return `DroppedBeforeWrite` rather than an error when
    /// the drop is recoverable. Returning `Written` means every byte was accepted.
    async fn write_access_unit(
        &mut self,
        access_unit: EncodedAccessUnit,
    ) -> Result<LaneWriteReport, MediaError>;
}
