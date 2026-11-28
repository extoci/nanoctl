use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nanoctl_core::video::{
    DeltaRepairRequest, MAX_ACCESS_UNIT_BYTES, MAX_VIDEO_PARTS, VIDEO_HEADER_BYTES, VideoChunk,
};
use tokio::time::{self, Instant as TokioInstant};

use crate::types::{EncodedAccessUnit, LaneWriteReport, LaneWriteStatus, LaneWriter, MediaError};

// Leave room for the video header and SCTP headers within WebRTC's 1191-byte SCTP MTU.
const CHUNK_PAYLOAD_BYTES: usize = 1_100;
const REFERENCE_ACK_GRACE: Duration = Duration::from_millis(350);
const DELTA_REPAIR_CACHE_FRAMES: usize = 16;
const VIDEO_REDUNDANCY_BUDGET_BYTES: usize = 8 * 1024;
const DELTA_REPAIR_CACHE_BYTES: usize = 128 * 1024;
const DELTA_REPAIR_MAX_AGE: Duration = Duration::from_millis(500);
const DELTA_REPAIR_ATTEMPTS: u8 = 2;
const DELTA_REPAIR_TOTAL_BYTES: usize =
    VIDEO_REDUNDANCY_BUDGET_BYTES * DELTA_REPAIR_ATTEMPTS as usize;
const RELIABLE_REPAIR_BYTES: usize = 4 * (CHUNK_PAYLOAD_BYTES + VIDEO_HEADER_BYTES);
const DELTA_QUEUE_TIME_MS: u64 = 75;
const DATAGRAM_BURST_CHUNKS: usize = 4;
const DATAGRAM_OVERHEAD_BYTES: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoLane {
    Reference,
    Delta,
}

/// Byte sink owned by the transport layer. Report the bytes still queued or
/// awaiting acknowledgment on the underlying lane. Capacity is a hard
/// backpressure budget, not an estimate: the writer either preflights and emits every
/// chunk of an access unit or drops it before emitting the first byte.
#[async_trait]
pub trait VideoChunkSink: Send {
    fn take_delta_repair_request(&mut self) -> Option<DeltaRepairRequest> {
        None
    }

    fn frame_pacing_bps(&self) -> Option<NonZeroU32> {
        None
    }

    async fn buffered_bytes(&self, lane: VideoLane) -> usize;
    async fn lane_capacity(&self, lane: VideoLane) -> usize;
    async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError>;
}

struct DatagramPacer {
    bitrate: NonZeroU32,
    chunks_in_burst: usize,
    next_burst: TokioInstant,
}

impl DatagramPacer {
    fn new(bitrate: NonZeroU32) -> Self {
        Self {
            bitrate,
            chunks_in_burst: 0,
            next_burst: TokioInstant::now(),
        }
    }

    async fn before_chunk(&mut self, bytes: usize) {
        if self.chunks_in_burst == DATAGRAM_BURST_CHUNKS {
            time::sleep_until(self.next_burst).await;
            self.chunks_in_burst = 0;
        }
        let bits = (bytes + DATAGRAM_OVERHEAD_BYTES) as u64 * 8;
        let spacing =
            Duration::from_nanos((bits * 1_000_000_000).div_ceil(self.bitrate.get().into()));
        self.next_burst = self.next_burst.max(TokioInstant::now()) + spacing;
        self.chunks_in_burst += 1;
    }
}

struct CachedDelta {
    sequence: u32,
    sent_at: Instant,
    wire_bytes: usize,
    repairs_remaining: u8,
    repair_bytes_remaining: usize,
    chunks: Vec<Vec<u8>>,
}

fn delta_queue_budget(capacity: usize, pacing_bps: Option<NonZeroU32>) -> usize {
    let half_capacity = capacity.div_ceil(2);
    pacing_bps.map_or(half_capacity, |bitrate| {
        let pacing_bytes =
            (u64::from(bitrate.get()) * DELTA_QUEUE_TIME_MS).div_ceil(8_000) as usize;
        half_capacity.min(pacing_bytes.max(CHUNK_PAYLOAD_BYTES + VIDEO_HEADER_BYTES))
    })
}

pub struct ChunkingLaneWriter<S> {
    sink: S,
    repeat_chunks: bool,
    last_reference_sent: Option<Instant>,
    reference_repair_bytes: usize,
    repair_cache: VecDeque<CachedDelta>,
    repair_cache_bytes: usize,
}

impl<S> ChunkingLaneWriter<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            repeat_chunks: false,
            last_reference_sent: None,
            reference_repair_bytes: 0,
            repair_cache: VecDeque::new(),
            repair_cache_bytes: 0,
        }
    }

    pub fn with_repeated_chunks(sink: S) -> Self {
        Self {
            repeat_chunks: true,
            ..Self::new(sink)
        }
    }

    pub fn into_sink(self) -> S {
        self.sink
    }

    fn reference_lane(&self, buffered: usize) -> Option<VideoLane> {
        // A bounded repair or a few control bytes can precede a new keyframe.
        if buffered <= self.reference_repair_bytes + VIDEO_HEADER_BYTES {
            return Some(VideoLane::Reference);
        }
        let lane = if self.repeat_chunks {
            VideoLane::Delta
        } else {
            VideoLane::Reference
        };
        self.last_reference_sent
            .is_none_or(|sent| sent.elapsed() >= REFERENCE_ACK_GRACE)
            .then_some(lane)
    }

    fn cache_delta(&mut self, sequence: u32, wire_bytes: usize, chunks: Vec<Vec<u8>>) {
        while (self.repair_cache.len() >= DELTA_REPAIR_CACHE_FRAMES
            || self.repair_cache_bytes + wire_bytes > DELTA_REPAIR_CACHE_BYTES)
            && let Some(oldest) = self.repair_cache.pop_front()
        {
            self.repair_cache_bytes -= oldest.wire_bytes;
        }
        self.repair_cache.push_back(CachedDelta {
            sequence,
            sent_at: Instant::now(),
            wire_bytes,
            repairs_remaining: DELTA_REPAIR_ATTEMPTS,
            repair_bytes_remaining: DELTA_REPAIR_TOTAL_BYTES,
            chunks,
        });
        self.repair_cache_bytes += wire_bytes;
    }
}

#[async_trait]
impl<S> LaneWriter for ChunkingLaneWriter<S>
where
    S: VideoChunkSink,
{
    async fn reference_ready(&mut self) -> bool {
        let buffered = self.sink.buffered_bytes(VideoLane::Reference).await;
        match self.reference_lane(buffered) {
            Some(VideoLane::Reference) => true,
            Some(VideoLane::Delta) => self.delta_ready().await,
            None => false,
        }
    }

    async fn delta_ready(&mut self) -> bool {
        // Stop before encoding through a congested queue. The encoder then
        // keeps its last reference and can resume with the newest capture.
        let capacity = self.sink.lane_capacity(VideoLane::Delta).await;
        let budget = delta_queue_budget(capacity, self.sink.frame_pacing_bps());
        self.sink.buffered_bytes(VideoLane::Delta).await < budget
    }

    async fn repair_pending_delta(&mut self) -> Result<Option<LaneWriteReport>, MediaError> {
        let Some(request) = self.sink.take_delta_repair_request() else {
            return Ok(None);
        };
        let stream_sequence = request.stream_sequence;
        let Some(slot) = self
            .repair_cache
            .iter()
            .position(|delta| delta.sequence == stream_sequence)
        else {
            tracing::debug!(target: "nanoctl_media::delta_repair", stream_sequence, reason = "not-cached", "v4 delta repair skipped");
            return Ok(None);
        };
        let delta = &self.repair_cache[slot];
        if delta.sent_at.elapsed() > DELTA_REPAIR_MAX_AGE {
            tracing::debug!(target: "nanoctl_media::delta_repair", stream_sequence, reason = "expired", "v4 delta repair skipped");
            return Ok(None);
        }
        let whole_delta = request.parts.is_empty();
        let repair_budget = if whole_delta {
            delta.repair_bytes_remaining
        } else {
            delta
                .repair_bytes_remaining
                .min(VIDEO_REDUNDANCY_BUDGET_BYTES)
        };
        let repair_chunks = if whole_delta {
            if delta.wire_bytes > repair_budget {
                tracing::debug!(target: "nanoctl_media::delta_repair", stream_sequence, required_bytes = delta.wire_bytes, repair_budget, reason = "byte-budget", "v4 delta repair skipped");
                return Ok(None);
            }
            delta.chunks.clone()
        } else {
            let mut chunks = Vec::with_capacity(request.parts.len());
            for part in request.parts {
                let Some(chunk) = delta.chunks.get(usize::from(part)) else {
                    return Ok(None);
                };
                chunks.push(chunk.clone());
            }
            chunks
        };
        let wire_bytes = repair_chunks.iter().map(Vec::len).sum::<usize>();
        let lane = if !whole_delta
            && delta.repairs_remaining < DELTA_REPAIR_ATTEMPTS
            && wire_bytes <= RELIABLE_REPAIR_BYTES
            && self.sink.buffered_bytes(VideoLane::Reference).await <= VIDEO_HEADER_BYTES
        {
            VideoLane::Reference
        } else {
            VideoLane::Delta
        };
        let capacity = self.sink.lane_capacity(lane).await;
        let buffered = self.sink.buffered_bytes(lane).await;
        if wire_bytes > repair_budget || wire_bytes > capacity.div_ceil(2).saturating_sub(buffered)
        {
            tracing::debug!(target: "nanoctl_media::delta_repair", stream_sequence, ?lane, wire_bytes, buffered, repair_budget, reason = "send-budget", "v4 delta repair skipped");
            return Ok(None);
        }
        let delta = &mut self.repair_cache[slot];
        delta.repairs_remaining -= 1;
        delta.repair_bytes_remaining -= wire_bytes;
        if delta.repairs_remaining == 0 || delta.repair_bytes_remaining == 0 {
            self.repair_cache_bytes -= delta.wire_bytes;
            self.repair_cache.remove(slot);
        }
        if lane == VideoLane::Reference {
            self.reference_repair_bytes = wire_bytes;
        }
        let mut pacer = if wire_bytes > VIDEO_REDUNDANCY_BUDGET_BYTES {
            self.sink.frame_pacing_bps().map(DatagramPacer::new)
        } else {
            None
        };
        for bytes in repair_chunks {
            if let Some(pacer) = &mut pacer {
                pacer.before_chunk(bytes.len()).await;
            }
            self.sink.write_chunk(lane, bytes).await?;
        }
        tracing::debug!(
            target: "nanoctl_media::delta_repair",
            stream_sequence, ?lane, wire_bytes, "v4 delta repair queued"
        );
        Ok(Some(LaneWriteReport {
            status: LaneWriteStatus::Written,
            buffered_bytes: self.sink.buffered_bytes(lane).await,
            wire_bytes,
        }))
    }

    async fn write_access_unit(
        &mut self,
        access_unit: EncodedAccessUnit,
    ) -> Result<LaneWriteReport, MediaError> {
        if access_unit.payload.is_empty() {
            return Err(MediaError::InvalidAccessUnit("empty payload"));
        }
        if access_unit.payload.len() > MAX_ACCESS_UNIT_BYTES {
            return Err(MediaError::InvalidAccessUnit("payload exceeds maximum"));
        }
        let parts = access_unit.payload.len().div_ceil(CHUNK_PAYLOAD_BYTES);

        let (lane, buffered_before) = if access_unit.keyframe {
            let buffered = self.sink.buffered_bytes(VideoLane::Reference).await;
            match self.reference_lane(buffered) {
                Some(VideoLane::Reference) => (VideoLane::Reference, buffered),
                // Keep at most one large reference on the reliable lane. Newer
                // recovery keyframes can expire without extending that backlog.
                Some(VideoLane::Delta) => (
                    VideoLane::Delta,
                    self.sink.buffered_bytes(VideoLane::Delta).await,
                ),
                None => {
                    return Ok(LaneWriteReport {
                        status: LaneWriteStatus::DroppedBeforeWrite,
                        buffered_bytes: buffered,
                        wire_bytes: 0,
                    });
                }
            }
        } else {
            (
                VideoLane::Delta,
                self.sink.buffered_bytes(VideoLane::Delta).await,
            )
        };
        let frame_wire_bytes = access_unit.payload.len() + parts * VIDEO_HEADER_BYTES;
        let large_frame = frame_wire_bytes > VIDEO_REDUNDANCY_BUDGET_BYTES;
        // A bulk copy of a reliable keyframe holds up fresh video on the same
        // SCTP association. Keep repetition for recovery keyframes sent only
        // on the expiring lane because they have no reliable copy.
        let allow_repeat = self.repeat_chunks
            && (!large_frame || (access_unit.keyframe && lane == VideoLane::Delta));
        let primary_budget = if allow_repeat && lane == VideoLane::Delta {
            frame_wire_bytes * 2
        } else {
            frame_wire_bytes
        };
        let available = self
            .sink
            .lane_capacity(lane)
            .await
            .saturating_sub(buffered_before);
        if primary_budget > available || parts > usize::from(MAX_VIDEO_PARTS) {
            return Ok(LaneWriteReport {
                status: LaneWriteStatus::DroppedBeforeWrite,
                buffered_bytes: buffered_before,
                wire_bytes: 0,
            });
        }

        // The extra copy of a reference can expire independently. A full delta
        // queue must never prevent writing the original to the reference lane.
        let repeat = allow_repeat
            && (lane == VideoLane::Delta
                || frame_wire_bytes
                    <= self
                        .sink
                        .lane_capacity(VideoLane::Delta)
                        .await
                        .saturating_sub(self.sink.buffered_bytes(VideoLane::Delta).await));
        let cache_delta = self.repeat_chunks
            && !access_unit.keyframe
            && frame_wire_bytes <= DELTA_REPAIR_CACHE_BYTES;
        let mut pacer = if access_unit.keyframe || large_frame {
            self.sink.frame_pacing_bps().map(DatagramPacer::new)
        } else {
            None
        };
        if lane == VideoLane::Reference {
            self.last_reference_sent = Some(Instant::now());
            self.reference_repair_bytes = 0;
        }
        let mut retained = Vec::new();
        let parts = parts as u16;
        for part in 0..parts {
            let start = usize::from(part) * CHUNK_PAYLOAD_BYTES;
            let end = (start + CHUNK_PAYLOAD_BYTES).min(access_unit.payload.len());
            let chunk = VideoChunk {
                stream_sequence: access_unit.stream_sequence,
                part,
                parts,
                keyframe: access_unit.keyframe,
                codec: access_unit.codec,
                capture_time_us: access_unit.capture_time_us,
                payload: access_unit.payload[start..end].to_vec(),
            };
            let bytes = chunk
                .encode()
                .map_err(|_| MediaError::InvalidAccessUnit("payload cannot be encoded"))?;
            if repeat || cache_delta {
                retained.push(bytes.clone());
            }
            if let Some(pacer) = &mut pacer {
                pacer.before_chunk(bytes.len()).await;
            }
            self.sink.write_chunk(lane, bytes).await?;
        }
        if repeat {
            // Give SCTP a chance to send the originals before it queues the copies,
            // including when the frame fits inside one short chunk.
            tokio::time::sleep(Duration::from_millis(8)).await;
            if cache_delta {
                self.cache_delta(
                    access_unit.stream_sequence,
                    frame_wire_bytes,
                    retained.clone(),
                );
            }
            for bytes in retained {
                if let Some(pacer) = &mut pacer {
                    pacer.before_chunk(bytes.len()).await;
                }
                self.sink.write_chunk(VideoLane::Delta, bytes).await?;
            }
        } else if cache_delta {
            self.cache_delta(access_unit.stream_sequence, frame_wire_bytes, retained);
        }

        let mut buffered_bytes = self.sink.buffered_bytes(lane).await;
        if lane == VideoLane::Reference && repeat {
            buffered_bytes = buffered_bytes.max(self.sink.buffered_bytes(VideoLane::Delta).await);
        }
        Ok(LaneWriteReport {
            status: LaneWriteStatus::Written,
            buffered_bytes,
            wire_bytes: frame_wire_bytes * (1 + usize::from(repeat)),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use async_trait::async_trait;
    use nanoctl_core::video::{DeltaRepairRequest, VideoChunk, VideoCodec};
    use tokio::time::Instant;

    use super::{ChunkingLaneWriter, DatagramPacer, VideoChunkSink, VideoLane, delta_queue_budget};
    use crate::types::{EncodedAccessUnit, LaneWriteStatus, LaneWriter, MediaError};

    struct MemorySink {
        chunks: Vec<(VideoLane, Vec<u8>)>,
        buffered: usize,
        capacity: usize,
        repair_request: Option<DeltaRepairRequest>,
    }

    #[async_trait]
    impl VideoChunkSink for MemorySink {
        fn take_delta_repair_request(&mut self) -> Option<DeltaRepairRequest> {
            self.repair_request.take()
        }

        async fn buffered_bytes(&self, lane: VideoLane) -> usize {
            self.buffered
                + self
                    .chunks
                    .iter()
                    .filter(|(queued_lane, _)| *queued_lane == lane)
                    .map(|(_, chunk)| chunk.len())
                    .sum::<usize>()
        }

        async fn lane_capacity(&self, _lane: VideoLane) -> usize {
            self.capacity
        }

        async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError> {
            self.chunks.push((lane, bytes));
            Ok(())
        }
    }

    fn access_unit(payload_len: usize, keyframe: bool) -> EncodedAccessUnit {
        EncodedAccessUnit {
            stream_sequence: 7,
            keyframe,
            codec: VideoCodec::H264,
            capture_time_us: 1234,
            capture_sequence: 3,
            payload: vec![0x5a; payload_len],
        }
    }

    #[test]
    fn the_capture_budget_caps_send_ahead_without_reducing_lane_capacity() {
        let capacity = 128 * 1024;
        assert_eq!(
            delta_queue_budget(capacity, NonZeroU32::new(2_400_000)),
            22_500
        );
        assert_eq!(
            delta_queue_budget(capacity, NonZeroU32::new(20_000_000)),
            65_536
        );
        assert_eq!(delta_queue_budget(capacity, None), 65_536);
    }

    #[tokio::test]
    async fn pacer_waits_after_four_datagrams_at_the_wire_rate() {
        let start = Instant::now();
        let mut pacer = DatagramPacer::new(NonZeroU32::new(2_400_000).unwrap());
        for _ in 0..4 {
            pacer.before_chunk(1120).await;
        }
        let deadline = pacer.next_burst;
        assert!(deadline >= start + Duration::from_millis(16));

        pacer.before_chunk(1120).await;
        assert!(Instant::now() >= deadline);
        assert_eq!(pacer.chunks_in_burst, 1);
    }

    #[tokio::test]
    async fn access_units_are_chunked_and_reference_units_use_reference_lane() {
        let payload_bytes = 1_100;
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: usize::MAX,
            repair_request: None,
        };
        let mut writer = ChunkingLaneWriter::new(sink);
        let report = writer
            .write_access_unit(access_unit(payload_bytes * 2 + 1, true))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::Written);
        let sink = writer.into_sink();
        assert_eq!(sink.chunks.len(), 3);
        assert!(
            sink.chunks
                .iter()
                .all(|(lane, _)| *lane == VideoLane::Reference)
        );

        let first = VideoChunk::parse(&sink.chunks[0].1).unwrap();
        assert_eq!(first.stream_sequence, 7);
        assert_eq!(first.part, 0);
        assert_eq!(first.parts, 3);
        assert_eq!(first.payload.len(), payload_bytes);
        assert!(first.keyframe);
        assert_eq!(first.capture_time_us, 1234);
        let last = VideoChunk::parse(&sink.chunks[2].1).unwrap();
        assert_eq!(last.part, 2);
        assert_eq!(last.payload.len(), 1);
    }

    #[tokio::test]
    async fn insufficient_lane_capacity_drops_before_first_byte() {
        let payload_len = 1_000;
        let wire_bytes = payload_len + 20;
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 1,
            capacity: wire_bytes,
            repair_request: None,
        };
        let mut writer = ChunkingLaneWriter::new(sink);
        let report = writer
            .write_access_unit(access_unit(payload_len, false))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::DroppedBeforeWrite);
        assert!(writer.into_sink().chunks.is_empty());
    }

    #[tokio::test]
    async fn a_recent_reference_in_flight_prevents_queueing_another() {
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: usize::MAX,
            repair_request: None,
        };
        let mut writer = ChunkingLaneWriter::new(sink);
        let first = writer
            .write_access_unit(access_unit(100, true))
            .await
            .unwrap();
        assert_eq!(first.status, LaneWriteStatus::Written);
        assert!(!writer.reference_ready().await);
        let next = writer
            .write_access_unit(access_unit(100, true))
            .await
            .unwrap();
        assert_eq!(next.status, LaneWriteStatus::DroppedBeforeWrite);
        assert_eq!(writer.into_sink().chunks.len(), 1);
    }

    #[tokio::test]
    async fn a_missing_reference_ack_allows_a_paced_retry_within_the_queue_budget() {
        let mut writer = ChunkingLaneWriter::new(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
            repair_request: None,
        });
        assert_eq!(
            writer
                .write_access_unit(access_unit(100, true))
                .await
                .unwrap()
                .status,
            LaneWriteStatus::Written,
        );
        assert!(!writer.reference_ready().await);

        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        assert!(writer.reference_ready().await);
        assert_eq!(
            writer
                .write_access_unit(access_unit(100, true))
                .await
                .unwrap()
                .status,
            LaneWriteStatus::Written,
        );
        assert!(!writer.reference_ready().await);

        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        assert_eq!(
            writer
                .write_access_unit(access_unit(100, true))
                .await
                .unwrap()
                .status,
            LaneWriteStatus::DroppedBeforeWrite,
        );
        assert_eq!(writer.into_sink().chunks.len(), 2);
    }

    #[tokio::test]
    async fn delta_units_use_delta_lane() {
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: usize::MAX,
            repair_request: None,
        };
        let mut writer = ChunkingLaneWriter::new(sink);
        writer
            .write_access_unit(access_unit(10, false))
            .await
            .unwrap();
        assert_eq!(writer.into_sink().chunks[0].0, VideoLane::Delta);
    }

    #[tokio::test]
    async fn a_delta_with_repetition_reserves_space_for_both_copies() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 239,
            repair_request: None,
        });
        let report = writer
            .write_access_unit(access_unit(100, false))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::DroppedBeforeWrite);
        assert_eq!(report.wire_bytes, 0);
        assert!(writer.into_sink().chunks.is_empty());

        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
            repair_request: None,
        });
        let report = writer
            .write_access_unit(access_unit(100, false))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::Written);
        assert_eq!(report.wire_bytes, 240);
        let sink = writer.into_sink();
        assert_eq!(sink.chunks.len(), 2);
        assert_eq!(sink.chunks[0], sink.chunks[1]);
    }

    #[tokio::test]
    async fn a_missing_delta_has_two_repairs_within_the_queue_budget() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 16 * 1024,
            repair_request: Some(DeltaRepairRequest {
                stream_sequence: 7,
                parts: vec![],
            }),
        });
        assert!(writer.repair_pending_delta().await.unwrap().is_none());
        writer
            .write_access_unit(access_unit(1400, false))
            .await
            .unwrap();

        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![],
        });
        let repair = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(repair.wire_bytes, 1440);
        assert_eq!(writer.sink.chunks.len(), 6);
        assert_eq!(writer.sink.chunks[..2], writer.sink.chunks[4..]);
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![],
        });
        let retry = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(retry.wire_bytes, 1440);
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![],
        });
        assert!(writer.repair_pending_delta().await.unwrap().is_none());

        let mut next = access_unit(1400, false);
        next.stream_sequence = 8;
        writer.write_access_unit(next).await.unwrap();
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 8,
            parts: vec![],
        });
        assert!(writer.repair_pending_delta().await.unwrap().is_none());
        assert_eq!(writer.sink.chunks.len(), 12);
    }

    #[tokio::test]
    async fn a_whole_delta_repair_can_use_the_shared_byte_budget() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 128 * 1024,
            repair_request: None,
        });
        let original = writer
            .write_access_unit(access_unit(12_100, false))
            .await
            .unwrap();
        assert_eq!(original.wire_bytes, 12_320);
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![],
        });
        let whole = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(whole.wire_bytes, original.wire_bytes);
        assert_eq!(writer.sink.chunks[..11], writer.sink.chunks[11..]);

        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![0, 1, 2, 3],
        });
        assert!(writer.repair_pending_delta().await.unwrap().is_none());
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![1, 2, 3],
        });
        let selective = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(selective.wire_bytes, 3360);
        assert_eq!(writer.repair_cache_bytes, 0);
    }

    #[tokio::test]
    async fn a_large_delta_is_sent_once_and_can_repair_missing_chunks() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 64 * 1024,
            repair_request: Some(DeltaRepairRequest {
                stream_sequence: 7,
                parts: vec![],
            }),
        });
        let original = writer
            .write_access_unit(access_unit(40_700, false))
            .await
            .unwrap();
        assert_eq!(original.status, LaneWriteStatus::Written);
        assert_eq!(original.wire_bytes, 41_440);
        assert_eq!(writer.sink.chunks.len(), 37);
        assert!(writer.repair_pending_delta().await.unwrap().is_none());
        // Model delivery of the original burst before the selective request arrives.
        writer.sink.chunks.clear();
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![4, 36],
        });
        let report = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(report.wire_bytes, 2240);
        let parts: Vec<_> = writer
            .sink
            .chunks
            .iter()
            .map(|(lane, bytes)| {
                let chunk = VideoChunk::parse(bytes).unwrap();
                (*lane, chunk.stream_sequence, chunk.part, chunk.parts)
            })
            .collect();
        assert_eq!(
            parts,
            [(VideoLane::Delta, 7, 4, 37), (VideoLane::Delta, 7, 36, 37)]
        );
        assert_eq!(writer.repair_cache_bytes, original.wire_bytes);
        writer.sink.repair_request = Some(DeltaRepairRequest {
            stream_sequence: 7,
            parts: vec![36],
        });
        let retry = writer.repair_pending_delta().await.unwrap().unwrap();
        assert_eq!(retry.wire_bytes, 1120);
        let (lane, bytes) = writer.sink.chunks.last().unwrap();
        assert_eq!(*lane, VideoLane::Reference);
        assert_eq!(VideoChunk::parse(bytes).unwrap().part, 36);
        assert_eq!(writer.repair_cache_bytes, 0);
        assert!(writer.reference_ready().await);

        let mut keyframe = access_unit(100, true);
        keyframe.stream_sequence = 8;
        writer.write_access_unit(keyframe).await.unwrap();
        assert_eq!(writer.sink.chunks[3].0, VideoLane::Reference);
        assert!(!writer.reference_ready().await);
    }

    #[tokio::test]
    async fn a_reference_copy_uses_the_delta_lane() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
            repair_request: None,
        });
        let report = writer
            .write_access_unit(access_unit(100, true))
            .await
            .unwrap();
        assert_eq!(report.wire_bytes, 240);
        let sink = writer.into_sink();
        assert_eq!(sink.chunks[0].0, VideoLane::Reference);
        assert_eq!(sink.chunks[1].0, VideoLane::Delta);
        assert_eq!(sink.chunks[0].1, sink.chunks[1].1);
    }

    #[tokio::test]
    async fn a_large_reliable_reference_leaves_the_delta_lane_free() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 128 * 1024,
            repair_request: None,
        });
        let report = writer
            .write_access_unit(access_unit(42_900, true))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::Written);
        assert_eq!(report.wire_bytes, 42_900 + 39 * 20);
        assert_eq!(writer.sink.chunks.len(), 39);
        assert!(
            writer
                .sink
                .chunks
                .iter()
                .all(|(lane, _)| *lane == VideoLane::Reference)
        );
    }

    #[tokio::test]
    async fn a_recovery_keyframe_uses_the_expiring_lane_while_the_reference_is_pending() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 1024,
            repair_request: None,
        });
        writer
            .write_access_unit(access_unit(100, true))
            .await
            .unwrap();
        assert!(!writer.reference_ready().await);
        writer.last_reference_sent = Some(super::Instant::now() - super::REFERENCE_ACK_GRACE);
        assert!(writer.reference_ready().await);

        let mut recovery = access_unit(100, true);
        recovery.stream_sequence = 8;
        let report = writer.write_access_unit(recovery).await.unwrap();
        assert_eq!(report.status, LaneWriteStatus::Written);
        assert_eq!(report.wire_bytes, 240);
        assert_eq!(writer.sink.chunks[0].0, VideoLane::Reference);
        assert_eq!(writer.sink.chunks.len(), 4);
        assert_eq!(writer.sink.chunks[2], writer.sink.chunks[3]);
        assert_eq!(writer.sink.chunks[2].0, VideoLane::Delta);
        let keyframe = VideoChunk::parse(&writer.sink.chunks[2].1).unwrap();
        assert_eq!(keyframe.stream_sequence, 8);
        assert!(keyframe.keyframe);
        assert!(writer.repair_cache.is_empty());
    }

    #[tokio::test]
    async fn a_full_delta_lane_does_not_block_the_reliable_reference() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
            repair_request: None,
        });
        writer
            .write_access_unit(access_unit(100, false))
            .await
            .unwrap();
        let report = writer
            .write_access_unit(access_unit(100, true))
            .await
            .unwrap();
        assert_eq!(report.status, LaneWriteStatus::Written);
        assert_eq!(report.wire_bytes, 120);
        let sink = writer.into_sink();
        assert_eq!(sink.chunks.len(), 3);
        assert_eq!(sink.chunks[2].0, VideoLane::Reference);
    }
}
