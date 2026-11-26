use async_trait::async_trait;
use nanoctl_core::video::{
    MAX_ACCESS_UNIT_BYTES, MAX_VIDEO_CHUNK_BYTES, MAX_VIDEO_PARTS, VIDEO_HEADER_BYTES, VideoChunk,
};

use crate::types::{EncodedAccessUnit, LaneWriteReport, LaneWriteStatus, LaneWriter, MediaError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoLane {
    Reference,
    Delta,
}

/// Byte sink owned by the transport layer. Implementations must expose the same
/// monotonic buffered-byte accounting as the underlying lane. Capacity is a hard
/// backpressure budget, not an estimate: the writer either preflights and emits every
/// chunk of an access unit or drops it before emitting the first byte.
#[async_trait]
pub trait VideoChunkSink: Send {
    fn buffered_bytes(&self, lane: VideoLane) -> usize;
    fn lane_capacity(&self, lane: VideoLane) -> usize;
    async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError>;
}

pub struct ChunkingLaneWriter<S> {
    sink: S,
}

impl<S> ChunkingLaneWriter<S> {
    pub fn new(sink: S) -> Self {
        Self { sink }
    }

    pub fn into_sink(self) -> S {
        self.sink
    }
}

#[async_trait]
impl<S> LaneWriter for ChunkingLaneWriter<S>
where
    S: VideoChunkSink,
{
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
        let parts = access_unit.payload.len().div_ceil(MAX_VIDEO_CHUNK_BYTES);
        if parts > usize::from(MAX_VIDEO_PARTS) {
            return Err(MediaError::InvalidAccessUnit(
                "payload requires too many chunks",
            ));
        }

        let lane = if access_unit.keyframe {
            VideoLane::Reference
        } else {
            VideoLane::Delta
        };
        let wire_bytes = access_unit.payload.len() + parts * VIDEO_HEADER_BYTES;
        let buffered_before = self.sink.buffered_bytes(lane);
        let available = self
            .sink
            .lane_capacity(lane)
            .saturating_sub(buffered_before);
        if wire_bytes > available {
            return Ok(LaneWriteReport {
                status: LaneWriteStatus::DroppedBeforeWrite,
                buffered_bytes: buffered_before,
            });
        }

        let parts = parts as u16;
        for part in 0..parts {
            let start = usize::from(part) * MAX_VIDEO_CHUNK_BYTES;
            let end = (start + MAX_VIDEO_CHUNK_BYTES).min(access_unit.payload.len());
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
            self.sink.write_chunk(lane, bytes).await?;
        }

        Ok(LaneWriteReport {
            status: LaneWriteStatus::Written,
            buffered_bytes: self.sink.buffered_bytes(lane),
        })
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use nanoctl_core::video::{MAX_VIDEO_CHUNK_BYTES, VideoChunk, VideoCodec};

    use super::{ChunkingLaneWriter, VideoChunkSink, VideoLane};
    use crate::types::{EncodedAccessUnit, LaneWriteStatus, LaneWriter, MediaError};

    struct MemorySink {
        chunks: Vec<(VideoLane, Vec<u8>)>,
        buffered: usize,
        capacity: usize,
    }

    #[async_trait]
    impl VideoChunkSink for MemorySink {
        fn buffered_bytes(&self, _lane: VideoLane) -> usize {
            self.buffered
        }

        fn lane_capacity(&self, _lane: VideoLane) -> usize {
            self.capacity
        }

        async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError> {
            self.buffered += bytes.len();
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
        let sink = MemorySink {
            chunks: Vec::new(),
            buffered: 0,
            capacity: usize::MAX,
        };
        let mut writer = ChunkingLaneWriter::new(sink);
        let report = writer
            .write_access_unit(access_unit(MAX_VIDEO_CHUNK_BYTES * 2 + 1, true))
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
        assert_eq!(first.payload.len(), MAX_VIDEO_CHUNK_BYTES);
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
}
