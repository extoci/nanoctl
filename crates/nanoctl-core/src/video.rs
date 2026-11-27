use crate::{
    WIRE_VERSION, WireError, push_u16, push_u32, push_u64, u16_at, u32_at, u64_at, versioned,
};

const KIND_VIDEO_CHUNK: u8 = 0x40;
const KIND_KEYFRAME_REQUEST: u8 = 0x41;
const KIND_SEQUENCE_WATERMARK: u8 = 0x42;
pub const MAX_VIDEO_CHUNK_BYTES: usize = 48 * 1_024;
pub const MAX_VIDEO_PARTS: u16 = 512;
pub const MAX_ACCESS_UNIT_BYTES: usize = 16 * 1_024 * 1_024;
pub const VIDEO_HEADER_BYTES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    fn from_wire(value: u8) -> Result<Self, WireError> {
        match value {
            1 => Ok(Self::H264),
            2 => Ok(Self::Hevc),
            3 => Ok(Self::Av1),
            _ => Err(WireError::InvalidValue("unknown video codec")),
        }
    }

    fn wire(self) -> u8 {
        match self {
            Self::H264 => 1,
            Self::Hevc => 2,
            Self::Av1 => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoControlMessage {
    KeyframeRequest,
    SequenceWatermark { next_sequence: u32 },
}

impl VideoControlMessage {
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::KeyframeRequest => vec![WIRE_VERSION, KIND_KEYFRAME_REQUEST],
            Self::SequenceWatermark { next_sequence } => {
                let mut out = vec![WIRE_VERSION, KIND_SEQUENCE_WATERMARK];
                push_u32(&mut out, next_sequence);
                out
            }
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        match (
            versioned(bytes, KIND_KEYFRAME_REQUEST..=KIND_SEQUENCE_WATERMARK)?,
            bytes.len(),
        ) {
            (KIND_KEYFRAME_REQUEST, 2) => Ok(Self::KeyframeRequest),
            (KIND_SEQUENCE_WATERMARK, 6) => Ok(Self::SequenceWatermark {
                next_sequence: u32_at(bytes, 2)?,
            }),
            _ => Err(WireError::InvalidLength),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoChunk {
    pub stream_sequence: u32,
    pub part: u16,
    pub parts: u16,
    pub keyframe: bool,
    pub codec: VideoCodec,
    /// Microseconds on the capture pipeline's monotonic clock. The epoch is local to a
    /// session generation; it is ordering/measuring metadata, not wall-clock time.
    pub capture_time_us: u64,
    pub payload: Vec<u8>,
}

impl VideoChunk {
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate_for_reassembly()?;
        let mut out = Vec::with_capacity(VIDEO_HEADER_BYTES + self.payload.len());
        out.push(WIRE_VERSION);
        out.push(KIND_VIDEO_CHUNK);
        push_u32(&mut out, self.stream_sequence);
        push_u16(&mut out, self.part);
        push_u16(&mut out, self.parts);
        out.push(self.codec.wire());
        out.push(u8::from(self.keyframe));
        push_u64(&mut out, self.capture_time_us);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        versioned(bytes, KIND_VIDEO_CHUNK..=KIND_VIDEO_CHUNK)?;
        if bytes.len() < VIDEO_HEADER_BYTES {
            return Err(WireError::Truncated);
        }
        let part = u16_at(bytes, 6)?;
        let parts = u16_at(bytes, 8)?;
        let flags = bytes[11];
        if flags & !1 != 0 {
            return Err(WireError::InvalidFlags);
        }
        let payload_len = bytes.len() - VIDEO_HEADER_BYTES;
        if parts == 0 || parts > MAX_VIDEO_PARTS || part >= parts {
            return Err(WireError::InvalidValue("invalid video chunk part count"));
        }
        if payload_len == 0 || payload_len > MAX_VIDEO_CHUNK_BYTES {
            return Err(WireError::InvalidLength);
        }
        Ok(Self {
            stream_sequence: u32_at(bytes, 2)?,
            part,
            parts,
            codec: VideoCodec::from_wire(bytes[10])?,
            keyframe: flags & 1 != 0,
            capture_time_us: u64_at(bytes, 12)?,
            payload: bytes[VIDEO_HEADER_BYTES..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub stream_sequence: u32,
    pub keyframe: bool,
    pub codec: VideoCodec,
    pub capture_time_us: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReassemblyOutcome {
    pub dropped_incomplete_sequence: Option<u32>,
    pub complete: Option<AccessUnit>,
}

#[derive(Debug)]
struct PartialAccessUnit {
    part_count: u16,
    received: u16,
    byte_len: usize,
    keyframe: bool,
    codec: VideoCodec,
    capture_time_us: u64,
    parts: Vec<Option<Vec<u8>>>,
}

const MAX_PENDING_REFERENCES: usize = 3;
const MAX_PENDING_DELTAS: usize = 16;

/// Reassembles a bounded set of access units across interleaved video chunks.
#[derive(Debug, Default)]
pub struct VideoReassembler {
    pending: std::collections::VecDeque<(u32, PartialAccessUnit)>,
    pending_bytes: usize,
}

impl VideoReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: VideoChunk) -> Result<ReassemblyOutcome, WireError> {
        chunk.validate_for_reassembly()?;
        let sequence = chunk.stream_sequence;
        let existing = self.pending.iter().position(|(seq, _)| *seq == sequence);
        if existing.is_none() && chunk.parts == 1 {
            return Ok(ReassemblyOutcome {
                complete: Some(AccessUnit {
                    stream_sequence: sequence,
                    keyframe: chunk.keyframe,
                    codec: chunk.codec,
                    capture_time_us: chunk.capture_time_us,
                    payload: chunk.payload,
                }),
                ..Default::default()
            });
        }

        if let Some(index) = existing {
            let current = &self.pending[index].1;
            if current.part_count != chunk.parts
                || current.keyframe != chunk.keyframe
                || current.codec != chunk.codec
                || current.capture_time_us != chunk.capture_time_us
            {
                return Err(WireError::InvalidValue(
                    "inconsistent video access-unit metadata",
                ));
            }
            if current.parts[usize::from(chunk.part)].is_some() {
                return Ok(ReassemblyOutcome::default());
            }
        }

        let eviction = if existing.is_none() {
            let limit = if chunk.keyframe {
                MAX_PENDING_REFERENCES
            } else {
                MAX_PENDING_DELTAS
            };
            let mut oldest = None;
            let mut count = 0;
            for (index, (_, pending)) in self.pending.iter().enumerate() {
                if pending.keyframe == chunk.keyframe {
                    oldest.get_or_insert(index);
                    count += 1;
                }
            }
            if count == limit { oldest } else { None }
        } else {
            None
        };
        let evicted_bytes = eviction.map_or(0, |index| self.pending[index].1.byte_len);
        let next_bytes = self.pending_bytes - evicted_bytes + chunk.payload.len();
        if next_bytes > MAX_ACCESS_UNIT_BYTES {
            return Err(WireError::InvalidValue(
                "video reassembly exceeds size limit",
            ));
        }

        let mut outcome = ReassemblyOutcome::default();
        let index = existing.unwrap_or_else(|| {
            if let Some(index) = eviction {
                outcome.dropped_incomplete_sequence =
                    self.pending.remove(index).map(|(seq, _)| seq);
            }
            self.pending.push_back((
                sequence,
                PartialAccessUnit {
                    part_count: chunk.parts,
                    received: 0,
                    byte_len: 0,
                    keyframe: chunk.keyframe,
                    codec: chunk.codec,
                    capture_time_us: chunk.capture_time_us,
                    parts: vec![None; usize::from(chunk.parts)],
                },
            ));
            self.pending.len() - 1
        });

        self.pending_bytes = next_bytes;
        let current = &mut self.pending[index].1;
        current.byte_len += chunk.payload.len();
        current.parts[usize::from(chunk.part)] = Some(chunk.payload);
        current.received += 1;
        if current.received == current.part_count {
            let (_, current) = self.pending.remove(index).expect("complete frame present");
            self.pending_bytes -= current.byte_len;
            let mut payload = Vec::with_capacity(current.byte_len);
            for part in current.parts {
                payload.extend_from_slice(&part.expect("received count implies part present"));
            }
            outcome.complete = Some(AccessUnit {
                stream_sequence: sequence,
                keyframe: current.keyframe,
                codec: current.codec,
                capture_time_us: current.capture_time_us,
                payload,
            });
        }
        Ok(outcome)
    }
}

impl VideoChunk {
    fn validate_for_reassembly(&self) -> Result<(), WireError> {
        if self.parts == 0 || self.parts > MAX_VIDEO_PARTS || self.part >= self.parts {
            return Err(WireError::InvalidValue("invalid video chunk part count"));
        }
        if self.payload.is_empty() || self.payload.len() > MAX_VIDEO_CHUNK_BYTES {
            return Err(WireError::InvalidLength);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceDecision {
    Accept,
    DropAwaitingKeyframe,
    RequestKeyframe,
}

/// Validates the encoded reference chain after transport reordering has been resolved.
/// It deliberately contains no timers or network behavior; callers decide when and how
/// to rate-limit the keyframe request represented by `RequestKeyframe`.
#[derive(Debug, Default)]
pub struct ReferenceTracker {
    expected_sequence: Option<u32>,
    awaiting_keyframe: bool,
}

impl ReferenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inspect(&mut self, stream_sequence: u32, keyframe: bool) -> ReferenceDecision {
        if keyframe {
            self.expected_sequence = Some(stream_sequence.wrapping_add(1));
            self.awaiting_keyframe = false;
            return ReferenceDecision::Accept;
        }
        if self.awaiting_keyframe {
            return ReferenceDecision::DropAwaitingKeyframe;
        }
        match self.expected_sequence {
            None => {
                self.awaiting_keyframe = true;
                ReferenceDecision::RequestKeyframe
            }
            Some(expected) if stream_sequence == expected => {
                self.expected_sequence = Some(expected.wrapping_add(1));
                ReferenceDecision::Accept
            }
            Some(_) => {
                self.awaiting_keyframe = true;
                ReferenceDecision::RequestKeyframe
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(sequence: u32, part: u16, parts: u16, keyframe: bool, payload: &[u8]) -> VideoChunk {
        VideoChunk {
            stream_sequence: sequence,
            part,
            parts,
            keyframe,
            codec: VideoCodec::H264,
            capture_time_us: 123,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn video_chunks_roundtrip() {
        let value = chunk(7, 1, 3, true, b"payload");
        assert_eq!(VideoChunk::parse(&value.encode().unwrap()), Ok(value));
    }

    #[test]
    fn video_parser_rejects_invalid_shapes() {
        assert_eq!(VideoChunk::parse(&[]), Err(WireError::Empty));
        assert_eq!(
            VideoChunk::parse(&[4, 1, 0, 0, 0, 0]),
            Err(WireError::UnknownKind(1))
        );

        let mut bad_flags = chunk(1, 0, 1, false, b"x").encode().unwrap();
        bad_flags[11] = 2;
        assert_eq!(VideoChunk::parse(&bad_flags), Err(WireError::InvalidFlags));

        let mut zero_parts = chunk(1, 0, 1, false, b"x").encode().unwrap();
        zero_parts[8] = 0;
        zero_parts[9] = 0;
        assert!(matches!(
            VideoChunk::parse(&zero_parts),
            Err(WireError::InvalidValue("invalid video chunk part count"))
        ));

        let empty_payload =
            chunk(1, 0, 1, false, b"x").encode().unwrap()[..VIDEO_HEADER_BYTES].to_vec();
        assert_eq!(
            VideoChunk::parse(&empty_payload),
            Err(WireError::InvalidLength)
        );
    }

    #[test]
    fn keyframe_request_roundtrips_and_rejects_payload() {
        let encoded = VideoControlMessage::KeyframeRequest.encode();
        assert_eq!(encoded, vec![4, 0x41]);
        assert_eq!(
            VideoControlMessage::parse(&encoded),
            Ok(VideoControlMessage::KeyframeRequest)
        );
        assert_eq!(
            VideoControlMessage::parse(&[4, 0x41, 0]),
            Err(WireError::InvalidLength)
        );
    }

    #[test]
    fn sequence_watermark_has_a_little_endian_next_sequence() {
        let message = VideoControlMessage::SequenceWatermark {
            next_sequence: 0x0102_0304,
        };
        assert_eq!(message.encode(), [4, 0x42, 4, 3, 2, 1]);
        assert_eq!(VideoControlMessage::parse(&message.encode()), Ok(message));
        assert_eq!(
            VideoControlMessage::parse(&[4, 0x42, 1]),
            Err(WireError::InvalidLength)
        );
    }

    #[test]
    fn reassembler_accepts_out_of_order_parts() {
        let mut reassembler = VideoReassembler::new();
        assert!(
            reassembler
                .push(chunk(9, 2, 3, true, b"c"))
                .unwrap()
                .complete
                .is_none()
        );
        assert!(
            reassembler
                .push(chunk(9, 0, 3, true, b"a"))
                .unwrap()
                .complete
                .is_none()
        );
        let outcome = reassembler.push(chunk(9, 1, 3, true, b"b")).unwrap();
        assert_eq!(
            outcome.complete,
            Some(AccessUnit {
                stream_sequence: 9,
                keyframe: true,
                codec: VideoCodec::H264,
                capture_time_us: 123,
                payload: b"abc".to_vec(),
            })
        );
    }

    #[test]
    fn reassembler_keeps_interleaved_frames_and_evicts_only_when_full() {
        let mut reassembler = VideoReassembler::new();
        reassembler.push(chunk(9, 0, 2, true, b"a")).unwrap();
        let complete = reassembler.push(chunk(10, 0, 1, true, b"b")).unwrap();
        assert!(complete.complete.is_some());
        assert_eq!(complete.dropped_incomplete_sequence, None);
        reassembler.push(chunk(11, 0, 2, true, b"c")).unwrap();
        let late = reassembler.push(chunk(9, 1, 2, true, b"d")).unwrap();
        assert_eq!(late.complete.unwrap().payload, b"ad");
        reassembler.push(chunk(12, 0, 2, true, b"e")).unwrap();
        reassembler.push(chunk(13, 0, 2, true, b"f")).unwrap();
        let overflow = reassembler.push(chunk(14, 0, 2, true, b"g")).unwrap();
        assert_eq!(overflow.dropped_incomplete_sequence, Some(11));
    }

    #[test]
    fn reassembler_does_not_evict_a_reference_to_hold_deltas() {
        let mut reassembler = VideoReassembler::new();
        reassembler.push(chunk(7, 0, 2, true, b"a")).unwrap();
        for sequence in 8..24 {
            let outcome = reassembler
                .push(chunk(sequence, 0, 2, false, b"b"))
                .unwrap();
            assert_eq!(outcome.dropped_incomplete_sequence, None);
        }
        let overflow = reassembler.push(chunk(24, 0, 2, false, b"b")).unwrap();
        assert_eq!(overflow.dropped_incomplete_sequence, Some(8));
        let reference = reassembler.push(chunk(7, 1, 2, true, b"c")).unwrap();
        assert_eq!(reference.complete.unwrap().payload, b"ac");
    }

    #[test]
    fn reference_tracker_accepts_keyframe_then_ordered_deltas() {
        let mut tracker = ReferenceTracker::new();
        assert_eq!(
            tracker.inspect(10, false),
            ReferenceDecision::RequestKeyframe
        );
        assert_eq!(
            tracker.inspect(10, false),
            ReferenceDecision::DropAwaitingKeyframe
        );
        assert_eq!(tracker.inspect(8, true), ReferenceDecision::Accept);
        assert_eq!(tracker.inspect(9, false), ReferenceDecision::Accept);
        assert_eq!(
            tracker.inspect(11, false),
            ReferenceDecision::RequestKeyframe
        );
        assert_eq!(
            tracker.inspect(12, false),
            ReferenceDecision::DropAwaitingKeyframe
        );
    }
}
