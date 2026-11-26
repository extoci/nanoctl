use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant as TokioInstant};

use crate::latest::{LatestSlot, PutOutcome};
use crate::scaling::FrameScaler;
use crate::types::{
    EncodeRequest, EncodedAccessUnit, LaneWriteStatus, LaneWriter, MediaError, MediaPipelineConfig,
    MediaPipelineSnapshot, MediaQuality, RawFrame, VideoEncoder, VideoSource,
};

pub struct MediaPipeline {
    shutdown_tx: watch::Sender<bool>,
    capture_task: Option<JoinHandle<Result<(), MediaError>>>,
    encode_task: Option<JoinHandle<Result<(), MediaError>>>,
    frame_slot: Arc<LatestSlot<RawFrame>>,
    keyframe_requested: Arc<AtomicBool>,
    metrics: Arc<Mutex<MediaPipelineSnapshot>>,
}

pub struct MediaPipelineReport {
    pub snapshot: MediaPipelineSnapshot,
    pub capture_result: Result<(), MediaError>,
    pub encode_result: Result<(), MediaError>,
}

impl MediaPipeline {
    pub fn start(
        config: MediaPipelineConfig,
        source: Box<dyn VideoSource>,
        encoder: Box<dyn VideoEncoder>,
        lane: Box<dyn LaneWriter>,
    ) -> Result<Self, MediaError> {
        Self::start_with_keyframe_requests(
            config,
            source,
            encoder,
            lane,
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Starts a pipeline whose reference recovery is driven by a transport-owned request flag.
    /// The flag remains shared after startup so data-channel control messages can request a
    /// keyframe without going through an unbounded command queue.
    pub fn start_with_keyframe_requests(
        config: MediaPipelineConfig,
        source: Box<dyn VideoSource>,
        encoder: Box<dyn VideoEncoder>,
        lane: Box<dyn LaneWriter>,
        keyframe_requested: Arc<AtomicBool>,
    ) -> Result<Self, MediaError> {
        Self::start_inner(config, source, encoder, lane, keyframe_requested, None)
    }

    /// Starts a pipeline that follows live viewer/link quality ceilings. Static config remains
    /// the hard upper bound; quality updates may only reduce resolution, rate or bitrate.
    pub fn start_with_quality(
        config: MediaPipelineConfig,
        source: Box<dyn VideoSource>,
        encoder: Box<dyn VideoEncoder>,
        lane: Box<dyn LaneWriter>,
        keyframe_requested: Arc<AtomicBool>,
        quality: watch::Receiver<MediaQuality>,
    ) -> Result<Self, MediaError> {
        Self::start_inner(
            config,
            source,
            encoder,
            lane,
            keyframe_requested,
            Some(quality),
        )
    }

    fn start_inner(
        config: MediaPipelineConfig,
        source: Box<dyn VideoSource>,
        encoder: Box<dyn VideoEncoder>,
        lane: Box<dyn LaneWriter>,
        keyframe_requested: Arc<AtomicBool>,
        quality: Option<watch::Receiver<MediaQuality>>,
    ) -> Result<Self, MediaError> {
        let config = config.validate()?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let frame_slot = Arc::new(LatestSlot::new());
        let metrics = Arc::new(Mutex::new(MediaPipelineSnapshot::default()));

        let capture_task = tokio::spawn(capture_loop(
            config,
            source,
            Arc::clone(&frame_slot),
            shutdown_rx,
            shutdown_tx.clone(),
            Arc::clone(&metrics),
            quality.clone(),
        ));
        let encode_task = tokio::spawn(encode_loop(EncodeLoopContext {
            config,
            frame_slot: Arc::clone(&frame_slot),
            encoder,
            lane,
            shutdown: shutdown_tx.subscribe(),
            shutdown_tx: shutdown_tx.clone(),
            keyframe_requested: Arc::clone(&keyframe_requested),
            metrics: Arc::clone(&metrics),
            quality,
        }));

        Ok(Self {
            shutdown_tx,
            capture_task: Some(capture_task),
            encode_task: Some(encode_task),
            frame_slot,
            keyframe_requested,
            metrics,
        })
    }

    pub fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Release);
    }

    pub fn snapshot(&self) -> MediaPipelineSnapshot {
        self.metrics.lock().expect("media metrics poisoned").clone()
    }

    pub async fn stop(mut self) -> MediaPipelineReport {
        let _ = self.shutdown_tx.send(true);
        self.frame_slot.close();

        let capture_result = join_media_task(
            self.capture_task
                .take()
                .expect("capture task missing")
                .await,
        );
        let encode_result =
            join_media_task(self.encode_task.take().expect("encode task missing").await);
        MediaPipelineReport {
            snapshot: self.snapshot(),
            capture_result,
            encode_result,
        }
    }
}

impl Drop for MediaPipeline {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        self.frame_slot.close();
    }
}

fn join_media_task(
    result: Result<Result<(), MediaError>, tokio::task::JoinError>,
) -> Result<(), MediaError> {
    match result {
        Ok(result) => result,
        Err(error) => Err(MediaError::TaskJoin(error.to_string())),
    }
}

fn record_metrics(
    metrics: &Mutex<MediaPipelineSnapshot>,
    update: impl FnOnce(&mut MediaPipelineSnapshot),
) {
    let mut snapshot = metrics.lock().expect("media metrics poisoned");
    update(&mut snapshot);
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.changed().await;
}

fn current_quality(
    config: MediaPipelineConfig,
    quality: &Option<watch::Receiver<MediaQuality>>,
) -> MediaQuality {
    quality.as_ref().map_or_else(
        || MediaQuality::from_config(config),
        |quality| quality.borrow().clamped(config),
    )
}

#[derive(Debug)]
struct LaneBitrateController {
    current_bps: u32,
    next_decrease: Instant,
    next_increase: Instant,
}

impl LaneBitrateController {
    const MIN_BPS: u32 = 350_000;
    const START_BPS: u32 = 2_000_000;

    fn new(ceiling_bps: u32) -> Self {
        Self {
            current_bps: Self::START_BPS
                .min(ceiling_bps)
                .max(Self::MIN_BPS.min(ceiling_bps)),
            next_decrease: Instant::now(),
            next_increase: Instant::now(),
        }
    }

    fn current(&mut self, ceiling_bps: u32) -> u32 {
        self.current_bps = self
            .current_bps
            .min(ceiling_bps)
            .max(Self::MIN_BPS.min(ceiling_bps));
        self.current_bps
    }

    fn update(
        &mut self,
        ceiling_bps: u32,
        status: LaneWriteStatus,
        buffered_bytes: usize,
        lane_capacity: usize,
    ) {
        let ceiling = ceiling_bps.max(Self::MIN_BPS);
        self.current_bps = self.current_bps.min(ceiling);
        let now = Instant::now();
        let high_water = lane_capacity / 2;
        let low_water = lane_capacity / 8;
        if status == LaneWriteStatus::DroppedBeforeWrite || buffered_bytes > high_water {
            if now >= self.next_decrease {
                self.current_bps = (self.current_bps * 2 / 3).max(Self::MIN_BPS);
                self.next_decrease = now + std::time::Duration::from_millis(250);
                self.next_increase = now + std::time::Duration::from_secs(1);
            }
        } else if buffered_bytes < low_water && now >= self.next_increase {
            let step = (self.current_bps / 4).max(100_000);
            self.current_bps = self.current_bps.saturating_add(step).min(ceiling);
            self.next_increase = now + std::time::Duration::from_millis(500);
        }
    }
}

async fn capture_loop(
    config: MediaPipelineConfig,
    mut source: Box<dyn VideoSource>,
    frame_slot: Arc<LatestSlot<RawFrame>>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    metrics: Arc<Mutex<MediaPipelineSnapshot>>,
    quality: Option<watch::Receiver<MediaQuality>>,
) -> Result<(), MediaError> {
    let result = capture_loop_inner(
        config,
        &mut source,
        &frame_slot,
        &mut shutdown,
        &metrics,
        &quality,
    )
    .await;
    frame_slot.close();
    if result.is_err() {
        let _ = shutdown_tx.send(true);
    }
    result
}

async fn capture_loop_inner(
    config: MediaPipelineConfig,
    source: &mut Box<dyn VideoSource>,
    frame_slot: &LatestSlot<RawFrame>,
    shutdown: &mut watch::Receiver<bool>,
    metrics: &Mutex<MediaPipelineSnapshot>,
    quality: &Option<watch::Receiver<MediaQuality>>,
) -> Result<(), MediaError> {
    let mut next_deadline = TokioInstant::now();
    let mut first_tick = true;
    let mut capture_sequence = 0_u64;

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let quality = current_quality(config, quality);
        let period = std::time::Duration::from_nanos(1_000_000_000 / u64::from(quality.frame_rate));
        let now = TokioInstant::now();
        if !first_tick {
            while next_deadline <= now {
                next_deadline += period;
            }
        }
        if next_deadline > now {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(shutdown) => return Ok(()),
                _ = time::sleep_until(next_deadline) => {}
            }
        }
        first_tick = false;
        record_metrics(metrics, |snapshot| snapshot.capture_ticks += 1);
        let next_frame = tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown) => return Ok(()),
            result = source.next_frame() => result,
        };
        next_deadline += period;
        match next_frame {
            Ok(Some(mut frame)) => {
                capture_sequence += 1;
                frame.capture_sequence = capture_sequence;
                let outcome = frame_slot.put(frame);
                record_metrics(metrics, |snapshot| {
                    snapshot.frames_captured += 1;
                    if outcome == PutOutcome::Replaced {
                        snapshot.frames_replaced_before_encode += 1;
                    }
                });
            }
            Ok(None) => return Ok(()),
            Err(error) => {
                record_metrics(metrics, |snapshot| snapshot.capture_errors += 1);
                return Err(error);
            }
        }
    }
}

struct EncodeLoopContext {
    config: MediaPipelineConfig,
    frame_slot: Arc<LatestSlot<RawFrame>>,
    encoder: Box<dyn VideoEncoder>,
    lane: Box<dyn LaneWriter>,
    shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    keyframe_requested: Arc<AtomicBool>,
    metrics: Arc<Mutex<MediaPipelineSnapshot>>,
    quality: Option<watch::Receiver<MediaQuality>>,
}

async fn encode_loop(context: EncodeLoopContext) -> Result<(), MediaError> {
    let EncodeLoopContext {
        config,
        frame_slot,
        mut encoder,
        mut lane,
        mut shutdown,
        shutdown_tx,
        keyframe_requested,
        metrics,
        quality,
    } = context;
    let result = encode_loop_inner(EncodeLoopParts {
        config,
        frame_slot: &frame_slot,
        encoder: &mut encoder,
        lane: &mut lane,
        shutdown: &mut shutdown,
        keyframe_requested: &keyframe_requested,
        metrics: &metrics,
        quality_updates: &quality,
    })
    .await;
    frame_slot.close();
    if result.is_err() {
        let _ = shutdown_tx.send(true);
    }
    result
}

struct EncodeLoopParts<'a> {
    config: MediaPipelineConfig,
    frame_slot: &'a LatestSlot<RawFrame>,
    encoder: &'a mut Box<dyn VideoEncoder>,
    lane: &'a mut Box<dyn LaneWriter>,
    shutdown: &'a mut watch::Receiver<bool>,
    keyframe_requested: &'a AtomicBool,
    metrics: &'a Mutex<MediaPipelineSnapshot>,
    quality_updates: &'a Option<watch::Receiver<MediaQuality>>,
}

async fn encode_loop_inner(parts: EncodeLoopParts<'_>) -> Result<(), MediaError> {
    let EncodeLoopParts {
        config,
        frame_slot,
        encoder,
        lane,
        shutdown,
        keyframe_requested,
        metrics,
        quality_updates,
    } = parts;
    let mut stream_sequence = 0_u32;
    let mut force_next_keyframe = true;
    let mut scaler = FrameScaler::default();
    let mut bitrate = LaneBitrateController::new(config.target_bitrate);
    let mut quality_dimensions: Option<(u32, u32)> = None;

    while let Some(frame) = frame_slot.recv().await {
        if *shutdown.borrow() {
            return Ok(());
        }
        record_metrics(metrics, |snapshot| snapshot.frames_taken_for_encode += 1);
        let quality = current_quality(config, quality_updates);
        let dimensions_changed = quality_dimensions != Some((quality.width, quality.height));
        quality_dimensions = Some((quality.width, quality.height));
        if !frame.has_new_damage
            && !force_next_keyframe
            && !keyframe_requested.load(Ordering::Acquire)
            && !dimensions_changed
        {
            record_metrics(metrics, |snapshot| snapshot.unchanged_frames_skipped += 1);
            continue;
        }
        let keyframe_was_requested = keyframe_requested.swap(false, Ordering::Acquire);
        let force_keyframe = force_next_keyframe || keyframe_was_requested || dimensions_changed;
        force_next_keyframe = false;
        stream_sequence = stream_sequence.wrapping_add(1);
        let capture_time_us = frame.capture_time_us;
        let capture_sequence = frame.capture_sequence;
        let encode_started = Instant::now();
        let frame = match scaler.scale(frame, quality.width, quality.height) {
            Ok(frame) => frame,
            Err(error) => {
                record_metrics(metrics, |snapshot| snapshot.encode_errors += 1);
                return Err(error);
            }
        };
        let request = EncodeRequest {
            stream_sequence,
            force_keyframe,
            target_bitrate: bitrate.current(quality.target_bitrate),
            codec: config.codec,
            frame,
        };
        let encoded = tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown) => return Ok(()),
            result = encoder.encode(request) => result,
        };
        let encode_micros = encode_started.elapsed().as_micros() as u64;
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                record_metrics(metrics, |snapshot| {
                    snapshot.encode_errors += 1;
                    snapshot.total_encode_micros += encode_micros;
                    snapshot.max_encode_micros = snapshot.max_encode_micros.max(encode_micros);
                });
                return Err(error);
            }
        };
        if encoded.payload.is_empty() {
            // Encoder rate-control skips are explicit drops, not corrupt partial access units.
            // The next emitted frame must restart the reference chain because the skipped input
            // still consumed a stream sequence number.
            record_metrics(metrics, |snapshot| {
                snapshot.encoder_skips += 1;
                snapshot.total_encode_micros += encode_micros;
                snapshot.max_encode_micros = snapshot.max_encode_micros.max(encode_micros);
            });
            force_next_keyframe = true;
            continue;
        }
        if force_keyframe && !encoded.keyframe {
            record_metrics(metrics, |snapshot| snapshot.encode_errors += 1);
            return Err(MediaError::EncoderContract(
                "forced keyframe request was not honored",
            ));
        }

        let access_unit = EncodedAccessUnit {
            stream_sequence,
            keyframe: encoded.keyframe,
            codec: config.codec,
            capture_time_us,
            capture_sequence,
            payload: encoded.payload,
        };
        let payload_len = access_unit.payload.len() as u64;
        record_metrics(metrics, |snapshot| {
            snapshot.access_units_encoded += 1;
            snapshot.keyframes_encoded += u64::from(access_unit.keyframe);
            snapshot.bytes_encoded += payload_len;
            snapshot.total_encode_micros += encode_micros;
            snapshot.max_encode_micros = snapshot.max_encode_micros.max(encode_micros);
        });

        // Lane writes are deliberately not cancellation-selected. A writer may emit
        // several wire chunks for one access unit, and cancelling between chunks could
        // publish a partial reference. Lane implementations must finish or reject one
        // access unit in bounded time and report that atomic outcome.
        let report = lane.write_access_unit(access_unit).await;
        match report {
            Ok(report) => {
                record_metrics(metrics, |snapshot| {
                    snapshot.lane_writes += 1;
                    snapshot.max_buffered_bytes = snapshot
                        .max_buffered_bytes
                        .max(report.buffered_bytes as u64);
                    if report.status == LaneWriteStatus::Written {
                        snapshot.bytes_written += payload_len;
                    } else {
                        snapshot.access_units_dropped_before_write += 1;
                    }
                });
                bitrate.update(
                    quality.target_bitrate,
                    report.status,
                    report.buffered_bytes,
                    report.lane_capacity,
                );
                if report.status == LaneWriteStatus::DroppedBeforeWrite {
                    force_next_keyframe = true;
                }
            }
            Err(error) => {
                record_metrics(metrics, |snapshot| snapshot.lane_write_errors += 1);
                return Err(error);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use nanoctl_core::video::VideoCodec;
    use tokio::sync::{Notify, mpsc, watch};

    use super::MediaPipeline;
    use crate::types::{
        EncodeRequest, EncodedAccessUnit, EncodedBitstream, LaneWriteReport, LaneWriteStatus,
        LaneWriter, MediaError, MediaPipelineConfig, MediaQuality, PixelFormat, RawFrame,
        VideoEncoder, VideoSource,
    };

    struct ChannelSource {
        receiver: mpsc::Receiver<RawFrame>,
    }

    #[async_trait]
    impl VideoSource for ChannelSource {
        async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError> {
            Ok(self.receiver.recv().await)
        }
    }

    struct TestEncoder;

    #[async_trait]
    impl VideoEncoder for TestEncoder {
        async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
            Ok(EncodedBitstream {
                keyframe: request.force_keyframe,
                payload: vec![if request.force_keyframe { 0x65 } else { 0x41 }],
            })
        }
    }

    struct DimensionEncoder {
        dimensions: Arc<Mutex<Vec<(u32, u32)>>>,
    }

    #[async_trait]
    impl VideoEncoder for DimensionEncoder {
        async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
            self.dimensions
                .lock()
                .expect("dimensions poisoned")
                .push((request.frame.width, request.frame.height));
            Ok(EncodedBitstream {
                keyframe: request.force_keyframe,
                payload: vec![0x65],
            })
        }
    }

    struct CollectLane {
        units: Arc<Mutex<Vec<EncodedAccessUnit>>>,
        notify: Arc<Notify>,
        status: LaneWriteStatus,
        buffered_bytes: usize,
    }

    #[async_trait]
    impl LaneWriter for CollectLane {
        async fn write_access_unit(
            &mut self,
            access_unit: EncodedAccessUnit,
        ) -> Result<LaneWriteReport, MediaError> {
            self.units
                .lock()
                .expect("collected units poisoned")
                .push(access_unit);
            self.notify.notify_one();
            Ok(LaneWriteReport {
                status: self.status,
                buffered_bytes: self.buffered_bytes,
                lane_capacity: 128 * 1024,
            })
        }
    }

    fn config() -> MediaPipelineConfig {
        MediaPipelineConfig {
            width: 64,
            height: 64,
            frame_rate: 240,
            target_bitrate: 1_000_000,
            codec: VideoCodec::H264,
        }
    }

    fn frame(capture_time_us: u64) -> RawFrame {
        RawFrame::new(
            capture_time_us,
            64,
            64,
            PixelFormat::Nv12,
            vec![0; 64 * 64 * 3 / 2],
        )
    }

    fn lane(
        status: LaneWriteStatus,
    ) -> (CollectLane, Arc<Mutex<Vec<EncodedAccessUnit>>>, Arc<Notify>) {
        let units = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        (
            CollectLane {
                units: Arc::clone(&units),
                notify: Arc::clone(&notify),
                status,
                buffered_bytes: 0,
            },
            units,
            notify,
        )
    }

    async fn wait_for_len(
        units: &Arc<Mutex<Vec<EncodedAccessUnit>>>,
        notify: &Notify,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if units.lock().expect("collected units poisoned").len() >= expected {
                    return;
                }
                notify.notified().await;
            }
        })
        .await
        .expect("timed out waiting for access unit");
    }

    #[tokio::test]
    async fn pipeline_scales_capture_to_configured_resolution_bounds() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let dimensions = Arc::new(Mutex::new(Vec::new()));
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(DimensionEncoder {
                dimensions: Arc::clone(&dimensions),
            }),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx
            .send(RawFrame::new(
                111,
                128,
                128,
                PixelFormat::Bgra32,
                vec![255; 128 * 128 * 4],
            ))
            .await
            .unwrap();
        wait_for_len(&units, &notify, 1).await;
        assert_eq!(
            *dimensions.lock().expect("dimensions poisoned"),
            vec![(64, 64)]
        );

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
    }

    struct QualityEncoder {
        observations: Arc<Mutex<Vec<(u32, u32, bool, u32)>>>,
    }

    #[async_trait]
    impl VideoEncoder for QualityEncoder {
        async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
            self.observations
                .lock()
                .expect("quality observations poisoned")
                .push((
                    request.frame.width,
                    request.frame.height,
                    request.force_keyframe,
                    request.target_bitrate,
                ));
            Ok(EncodedBitstream {
                keyframe: request.force_keyframe,
                payload: vec![if request.force_keyframe { 0x65 } else { 0x41 }],
            })
        }
    }

    fn rgba_frame(capture_time_us: u64) -> RawFrame {
        RawFrame::new(
            capture_time_us,
            64,
            64,
            PixelFormat::Bgra32,
            vec![255; 64 * 64 * 4],
        )
    }

    #[tokio::test]
    async fn live_quality_updates_scale_and_force_keyframe() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let observations = Arc::new(Mutex::new(Vec::new()));
        let config = config();
        let (quality_tx, quality_rx) = watch::channel(MediaQuality::from_config(config));
        let pipeline = MediaPipeline::start_with_quality(
            config,
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(QualityEncoder {
                observations: Arc::clone(&observations),
            }),
            Box::new(collecting),
            Arc::new(AtomicBool::new(false)),
            quality_rx,
        )
        .unwrap();

        frames_tx.send(rgba_frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        quality_tx.send_replace(MediaQuality {
            width: 32,
            height: 32,
            frame_rate: 30,
            target_bitrate: 500_000,
        });
        frames_tx.send(rgba_frame(222)).await.unwrap();
        wait_for_len(&units, &notify, 2).await;

        let observed = observations
            .lock()
            .expect("quality observations poisoned")
            .clone();
        assert_eq!(observed[1].0, 32);
        assert_eq!(observed[1].1, 32);
        assert!(observed[1].2);
        assert_eq!(observed[1].3, 500_000);

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
    }

    #[tokio::test]
    async fn congested_lane_reduces_next_encoder_bitrate() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let units = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let congested = CollectLane {
            units: Arc::clone(&units),
            notify: Arc::clone(&notify),
            status: LaneWriteStatus::Written,
            buffered_bytes: 100 * 1024,
        };
        let observations = Arc::new(Mutex::new(Vec::new()));
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(QualityEncoder {
                observations: Arc::clone(&observations),
            }),
            Box::new(congested),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        frames_tx.send(frame(222)).await.unwrap();
        wait_for_len(&units, &notify, 2).await;

        let observed = observations
            .lock()
            .expect("quality observations poisoned")
            .clone();
        assert_eq!(observed[0].3, 1_000_000);
        assert!(
            observed[1].3 < 800_000,
            "congestion did not lower bitrate: {observed:?}"
        );

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
    }

    #[tokio::test]
    async fn high_bitrate_config_starts_at_conservative_probe() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let observations = Arc::new(Mutex::new(Vec::new()));
        let pipeline = MediaPipeline::start(
            MediaPipelineConfig {
                target_bitrate: 100_000_000,
                ..config()
            },
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(QualityEncoder {
                observations: Arc::clone(&observations),
            }),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        let observed = observations
            .lock()
            .expect("quality observations poisoned")
            .clone();
        assert_eq!(observed[0].3, 2_000_000);

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
    }

    #[tokio::test]
    async fn pipeline_preserves_capture_time_and_applies_keyframe_requests() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(TestEncoder),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        frames_tx.send(frame(222)).await.unwrap();
        wait_for_len(&units, &notify, 2).await;
        pipeline.request_keyframe();
        frames_tx.send(frame(333)).await.unwrap();
        wait_for_len(&units, &notify, 3).await;

        let observed = {
            let written = units.lock().expect("collected units poisoned");
            (
                written[0].stream_sequence,
                written[0].keyframe,
                written[0].capture_time_us,
                written[1].stream_sequence,
                written[1].keyframe,
                written[1].capture_time_us,
                written[2].stream_sequence,
                written[2].keyframe,
                written[2].capture_time_us,
            )
        };
        assert_eq!(observed, (1, true, 111, 2, false, 222, 3, true, 333));

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
        assert_eq!(report.snapshot.frames_captured, 3);
        assert_eq!(report.snapshot.frames_taken_for_encode, 3);
        assert_eq!(report.snapshot.access_units_encoded, 3);
        assert_eq!(report.snapshot.keyframes_encoded, 2);
    }

    #[tokio::test]
    async fn dropped_access_unit_forces_next_keyframe() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::DroppedBeforeWrite);
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(TestEncoder),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        frames_tx.send(frame(222)).await.unwrap();
        wait_for_len(&units, &notify, 2).await;

        let both_keyframes = {
            let written = units.lock().expect("collected units poisoned");
            written[0].keyframe && written[1].keyframe
        };
        assert!(both_keyframes);

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
        assert_eq!(report.snapshot.access_units_dropped_before_write, 2);
        assert_eq!(report.snapshot.keyframes_encoded, 2);
    }

    struct SkipFirstEncoder {
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl VideoEncoder for SkipFirstEncoder {
        async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
            let attempt = self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
            if attempt == 0 {
                return Ok(EncodedBitstream {
                    keyframe: false,
                    payload: Vec::new(),
                });
            }
            Ok(EncodedBitstream {
                keyframe: request.force_keyframe,
                payload: vec![if request.force_keyframe { 0x65 } else { 0x41 }],
            })
        }
    }

    #[tokio::test]
    async fn encoder_skip_is_visible_and_forces_next_keyframe() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let attempts = Arc::new(AtomicUsize::new(0));
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(SkipFirstEncoder {
                attempts: Arc::clone(&attempts),
            }),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while attempts.load(AtomicOrdering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        frames_tx.send(frame(222)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;

        let first_written = {
            let written = units.lock().expect("collected units poisoned");
            (written[0].stream_sequence, written[0].keyframe)
        };
        assert_eq!(first_written, (2, true));

        let report = pipeline.stop().await;
        assert_eq!(report.snapshot.encoder_skips, 1);
        assert_eq!(report.snapshot.access_units_encoded, 1);
        assert_eq!(report.snapshot.keyframes_encoded, 1);
    }

    #[tokio::test]
    async fn unchanged_frames_skip_encode_but_keyframe_requests_still_emit() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(TestEncoder),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        wait_for_len(&units, &notify, 1).await;
        let mut unchanged = frame(222);
        unchanged.has_new_damage = false;
        frames_tx.send(unchanged).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while pipeline.snapshot().unchanged_frames_skipped == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("unchanged frame was not skipped");
        assert_eq!(units.lock().expect("collected units poisoned").len(), 1);

        pipeline.request_keyframe();
        let mut unchanged = frame(333);
        unchanged.has_new_damage = false;
        frames_tx.send(unchanged).await.unwrap();
        wait_for_len(&units, &notify, 2).await;
        let second = {
            let written = units.lock().expect("collected units poisoned");
            (written[1].stream_sequence, written[1].keyframe)
        };
        assert_eq!(second, (2, true));

        let report = pipeline.stop().await;
        assert_eq!(report.snapshot.unchanged_frames_skipped, 1);
        assert_eq!(report.snapshot.access_units_encoded, 2);
    }

    struct NonKeyframeEncoder {
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl VideoEncoder for NonKeyframeEncoder {
        async fn encode(
            &mut self,
            _request: EncodeRequest,
        ) -> Result<EncodedBitstream, MediaError> {
            self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(EncodedBitstream {
                keyframe: false,
                payload: vec![0x41],
            })
        }
    }

    #[tokio::test]
    async fn unhonored_forced_keyframe_is_fatal_before_lane_write() {
        let (frames_tx, frames_rx) = mpsc::channel(1);
        let (collecting, units, _notify) = lane(LaneWriteStatus::Written);
        let attempts = Arc::new(AtomicUsize::new(0));
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(NonKeyframeEncoder {
                attempts: Arc::clone(&attempts),
            }),
            Box::new(collecting),
        )
        .unwrap();

        frames_tx.send(frame(111)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while attempts.load(AtomicOrdering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let report = pipeline.stop().await;
        assert!(matches!(
            report.encode_result,
            Err(MediaError::EncoderContract(
                "forced keyframe request was not honored"
            ))
        ));
        assert!(units.lock().expect("collected units poisoned").is_empty());
        assert_eq!(report.snapshot.encode_errors, 1);
    }

    struct SlowFirstSource {
        first: bool,
    }

    #[async_trait]
    impl VideoSource for SlowFirstSource {
        async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError> {
            if self.first {
                self.first = false;
                tokio::time::sleep(Duration::from_millis(45)).await;
            }
            Ok(Some(frame(1)))
        }
    }

    #[tokio::test]
    async fn missed_capture_deadlines_are_skipped_not_caught_up() {
        let (collecting, _units, _notify) = lane(LaneWriteStatus::Written);
        let pipeline = MediaPipeline::start(
            MediaPipelineConfig {
                frame_rate: 100,
                ..config()
            },
            Box::new(SlowFirstSource { first: true }),
            Box::new(TestEncoder),
            Box::new(collecting),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(65)).await;
        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(
            report.snapshot.capture_ticks <= 3,
            "capture ticks bursted after a missed deadline: {:?}",
            report.snapshot
        );
    }
}
