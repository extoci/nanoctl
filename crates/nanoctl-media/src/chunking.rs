use std::time::{Duration, Instant};

use async_trait::async_trait;
use nanoctl_core::video::{MAX_ACCESS_UNIT_BYTES, MAX_VIDEO_PARTS, VIDEO_HEADER_BYTES, VideoChunk};

use crate::types::{EncodedAccessUnit, LaneWriteReport, LaneWriteStatus, LaneWriter, MediaError};

// Leave room for the video header and SCTP headers within WebRTC's 1191-byte SCTP MTU.
const CHUNK_PAYLOAD_BYTES: usize = 1_100;
const REFERENCE_ACK_GRACE: Duration = Duration::from_millis(350);

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
    async fn buffered_bytes(&self, lane: VideoLane) -> usize;
    async fn lane_capacity(&self, lane: VideoLane) -> usize;
    async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError>;
}

pub struct ChunkingLaneWriter<S> {
    sink: S,
    repeat_chunks: bool,
    last_reference_sent: Option<Instant>,
}

impl<S> ChunkingLaneWriter<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            repeat_chunks: false,
            last_reference_sent: None,
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

    fn reference_can_write(&self, buffered: usize) -> bool {
        // A few control bytes can remain queued. If the previous reference still has
        // chunks awaiting acknowledgment, allow a paced retry instead of waiting forever.
        buffered <= VIDEO_HEADER_BYTES
            || self
                .last_reference_sent
                .is_none_or(|sent| sent.elapsed() >= REFERENCE_ACK_GRACE)
    }
}

#[async_trait]
impl<S> LaneWriter for ChunkingLaneWriter<S>
where
    S: VideoChunkSink,
{
    async fn reference_ready(&mut self) -> bool {
        let buffered = self.sink.buffered_bytes(VideoLane::Reference).await;
        self.reference_can_write(buffered)
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

        let lane = if access_unit.keyframe {
            VideoLane::Reference
        } else {
            VideoLane::Delta
        };
        let frame_wire_bytes = access_unit.payload.len() + parts * VIDEO_HEADER_BYTES;
        let primary_budget = if self.repeat_chunks && lane == VideoLane::Delta {
            frame_wire_bytes * 2
        } else {
            frame_wire_bytes
        };
        let buffered_before = self.sink.buffered_bytes(lane).await;
        let available = self
            .sink
            .lane_capacity(lane)
            .await
            .saturating_sub(buffered_before);
        if primary_budget > available
            || parts > usize::from(MAX_VIDEO_PARTS)
            || (lane == VideoLane::Reference && !self.reference_can_write(buffered_before))
        {
            return Ok(LaneWriteReport {
                status: LaneWriteStatus::DroppedBeforeWrite,
                buffered_bytes: buffered_before,
                wire_bytes: 0,
            });
        }

        // The extra copy of a reference can expire independently. A full delta
        // queue must never prevent writing the original to the reference lane.
        let repeat = self.repeat_chunks
            && (lane == VideoLane::Delta
                || frame_wire_bytes
                    <= self
                        .sink
                        .lane_capacity(VideoLane::Delta)
                        .await
                        .saturating_sub(self.sink.buffered_bytes(VideoLane::Delta).await));
        let mut repeated = Vec::new();
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
            if repeat {
                repeated.push(bytes.clone());
            }
            self.sink.write_chunk(lane, bytes).await?;
        }
        if repeat {
            // Give SCTP a chance to send the originals before it queues the copies,
            // including when the frame fits inside one short chunk.
            tokio::time::sleep(Duration::from_millis(8)).await;
            for bytes in repeated {
                self.sink.write_chunk(VideoLane::Delta, bytes).await?;
            }
        }
        if lane == VideoLane::Reference {
            self.last_reference_sent = Some(Instant::now());
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
    use async_trait::async_trait;
    use nanoctl_core::video::{VideoChunk, VideoCodec};

    use super::{ChunkingLaneWriter, VideoChunkSink, VideoLane};
    use crate::types::{EncodedAccessUnit, LaneWriteStatus, LaneWriter, MediaError};

    struct MemorySink {
        chunks: Vec<(VideoLane, Vec<u8>)>,
        buffered: usize,
        capacity: usize,
    }

    #[async_trait]
    impl VideoChunkSink for MemorySink {
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

    #[tokio::test]
    async fn access_units_are_chunked_and_reference_units_use_reference_lane() {
        let payload_bytes = 1_100;
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: usize::MAX,
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
    async fn a_reference_copy_uses_the_delta_lane() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
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
    async fn a_full_delta_lane_does_not_block_the_reliable_reference() {
        let mut writer = ChunkingLaneWriter::with_repeated_chunks(MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: 240,
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
