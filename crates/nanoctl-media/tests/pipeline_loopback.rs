use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nanoctl_core::video::{
    ReferenceDecision, ReferenceTracker, VideoChunk, VideoCodec, VideoReassembler,
};
use nanoctl_media::{
    ChunkingLaneWriter, MediaError, MediaPipeline, MediaPipelineConfig, OpenH264Encoder,
    PixelFormat, RawFrame, VideoChunkSink, VideoLane, VideoSource,
};

struct ScriptedSource {
    frames: VecDeque<RawFrame>,
}

#[async_trait]
impl VideoSource for ScriptedSource {
    async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError> {
        Ok(self.frames.pop_front())
    }
}

type RecordedChunks = Arc<Mutex<Vec<(VideoLane, Vec<u8>)>>>;

#[derive(Clone)]
struct SharedMemorySink {
    chunks: RecordedChunks,
    buffered: Arc<AtomicUsize>,
}

#[async_trait]
impl VideoChunkSink for SharedMemorySink {
    fn buffered_bytes(&self, _lane: VideoLane) -> usize {
        self.buffered.load(Ordering::Acquire)
    }

    fn lane_capacity(&self, _lane: VideoLane) -> usize {
        usize::MAX
    }

    async fn write_chunk(&mut self, lane: VideoLane, bytes: Vec<u8>) -> Result<(), MediaError> {
        self.buffered.fetch_add(bytes.len(), Ordering::AcqRel);
        self.chunks
            .lock()
            .expect("loopback chunks poisoned")
            .push((lane, bytes));
        Ok(())
    }
}

fn frame(index: u64) -> RawFrame {
    let width = 128_usize;
    let height = 64_usize;
    let mut pixels = vec![0; width * height * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[0] = index as u8;
        pixel[1] = 255 - index as u8;
        pixel[2] = 80;
        pixel[3] = 255;
    }
    RawFrame::new(
        index * 16_000,
        width as u32,
        height as u32,
        PixelFormat::Rgba32,
        pixels,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openh264_pipeline_chunks_reassemble_into_a_valid_reference_chain() {
    let source = ScriptedSource {
        frames: (0..4).map(frame).collect(),
    };
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let sink = SharedMemorySink {
        chunks: Arc::clone(&chunks),
        buffered: Arc::new(AtomicUsize::new(0)),
    };
    let pipeline = MediaPipeline::start(
        MediaPipelineConfig {
            width: 128,
            height: 64,
            frame_rate: 15,
            target_bitrate: 4_000_000,
            codec: VideoCodec::H264,
        },
        Box::new(source),
        Box::new(OpenH264Encoder::new(4_000_000, 15).unwrap()),
        Box::new(ChunkingLaneWriter::new(sink)),
    )
    .unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        while pipeline.snapshot().frames_taken_for_encode < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pipeline did not encode scripted frames");
    let report = tokio::time::timeout(Duration::from_secs(5), pipeline.stop())
        .await
        .expect("pipeline did not finish");
    assert!(report.capture_result.is_ok());
    assert!(report.encode_result.is_ok());
    assert_eq!(report.snapshot.capture_errors, 0);
    assert_eq!(report.snapshot.encode_errors, 0);
    assert_eq!(report.snapshot.lane_write_errors, 0);
    assert!(report.snapshot.access_units_encoded > 0);

    let chunks = chunks.lock().expect("loopback chunks poisoned").clone();
    assert!(!chunks.is_empty());
    assert_eq!(chunks[0].0, VideoLane::Reference);

    let mut reassembler = VideoReassembler::new();
    let mut references = ReferenceTracker::new();
    let mut complete_units = 0_u32;
    for (_lane, bytes) in chunks {
        let chunk = VideoChunk::parse(&bytes).unwrap();
        let outcome = reassembler.push(chunk).unwrap();
        if let Some(access_unit) = outcome.complete {
            assert_eq!(
                references.inspect(access_unit.stream_sequence, access_unit.keyframe),
                ReferenceDecision::Accept
            );
            assert!(
                access_unit.payload.starts_with(&[0, 0, 1])
                    || access_unit.payload.starts_with(&[0, 0, 0, 1])
            );
            complete_units += 1;
        }
    }
    assert!(complete_units >= 2);
}
