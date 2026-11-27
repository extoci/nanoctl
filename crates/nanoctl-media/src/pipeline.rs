use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant as TokioInstant};

use crate::latest::{LatestSlot, PutOutcome};
use crate::scaling::FrameScaler;
use crate::types::{
    EncodeRequest, EncodedAccessUnit, LaneWriteStatus, LaneWriter, MediaError, MediaPipelineConfig,
    MediaPipelineSnapshot, RawFrame, VideoEncoder, VideoSource,
};

const MIN_FORCED_KEYFRAME_INTERVAL: Duration = Duration::from_millis(350);

pub struct MediaPipeline {
    settings: watch::Sender<MediaPipelineConfig>,
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
        let config = config.validate()?;
        let (settings, updates) = watch::channel(config);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let frame_slot = Arc::new(LatestSlot::new());
        let metrics = Arc::new(Mutex::new(MediaPipelineSnapshot::default()));

        let capture_task = tokio::spawn(capture_loop(
            updates.clone(),
            source,
            Arc::clone(&frame_slot),
            shutdown_rx,
            shutdown_tx.clone(),
            Arc::clone(&metrics),
        ));
        let encode_task = tokio::spawn(encode_loop(EncodeLoopContext {
            updates,
            frame_slot: Arc::clone(&frame_slot),
            encoder,
            lane,
            shutdown: shutdown_tx.subscribe(),
            shutdown_tx: shutdown_tx.clone(),
            keyframe_requested: Arc::clone(&keyframe_requested),
            metrics: Arc::clone(&metrics),
        }));

        Ok(Self {
            settings,
            shutdown_tx,
            capture_task: Some(capture_task),
            encode_task: Some(encode_task),
            frame_slot,
            keyframe_requested,
            metrics,
        })
    }

    pub fn update_settings(&self, config: MediaPipelineConfig) -> Result<(), MediaError> {
        let config = config.validate()?;
        if config.codec != self.settings.borrow().codec {
            return Err(MediaError::InvalidConfig(
                "codec cannot change within one media pipeline",
            ));
        }
        self.settings.send_if_modified(|current| {
            if *current == config {
                false
            } else {
                *current = config;
                true
            }
        });
        Ok(())
    }

    pub fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.capture_task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            || self
                .encode_task
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
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

async fn capture_loop(
    updates: watch::Receiver<MediaPipelineConfig>,
    mut source: Box<dyn VideoSource>,
    frame_slot: Arc<LatestSlot<RawFrame>>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    metrics: Arc<Mutex<MediaPipelineSnapshot>>,
) -> Result<(), MediaError> {
    let result =
        capture_loop_inner(&updates, &mut source, &frame_slot, &mut shutdown, &metrics).await;
    frame_slot.close();
    if result.is_err() {
        let _ = shutdown_tx.send(true);
    }
    result
}

async fn capture_loop_inner(
    updates: &watch::Receiver<MediaPipelineConfig>,
    source: &mut Box<dyn VideoSource>,
    frame_slot: &LatestSlot<RawFrame>,
    shutdown: &mut watch::Receiver<bool>,
    metrics: &Mutex<MediaPipelineSnapshot>,
) -> Result<(), MediaError> {
    let mut next_deadline = TokioInstant::now();
    let mut first_tick = true;
    let mut capture_sequence = 0_u64;

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let period = updates.borrow().frame_interval();
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
    updates: watch::Receiver<MediaPipelineConfig>,
    frame_slot: Arc<LatestSlot<RawFrame>>,
    encoder: Box<dyn VideoEncoder>,
    lane: Box<dyn LaneWriter>,
    shutdown: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    keyframe_requested: Arc<AtomicBool>,
    metrics: Arc<Mutex<MediaPipelineSnapshot>>,
}

async fn encode_loop(context: EncodeLoopContext) -> Result<(), MediaError> {
    let EncodeLoopContext {
        updates,
        frame_slot,
        mut encoder,
        mut lane,
        mut shutdown,
        shutdown_tx,
        keyframe_requested,
        metrics,
    } = context;
    let result = encode_loop_inner(
        &updates,
        &frame_slot,
        &mut encoder,
        &mut lane,
        &mut shutdown,
        &keyframe_requested,
        &metrics,
    )
    .await;
    frame_slot.close();
    if result.is_err() {
        let _ = shutdown_tx.send(true);
    }
    result
}

async fn encode_loop_inner(
    updates: &watch::Receiver<MediaPipelineConfig>,
    frame_slot: &LatestSlot<RawFrame>,
    encoder: &mut Box<dyn VideoEncoder>,
    lane: &mut Box<dyn LaneWriter>,
    shutdown: &mut watch::Receiver<bool>,
    keyframe_requested: &AtomicBool,
    metrics: &Mutex<MediaPipelineSnapshot>,
) -> Result<(), MediaError> {
    let mut stream_sequence = 0_u32;
    let mut force_next_keyframe = true;
    let mut next_forced_keyframe = TokioInstant::now();
    let mut last_dimensions = None;
    let mut scaler = FrameScaler::default();
    let mut deferred_reference = None;

    loop {
        let (frame, newly_captured) = match deferred_reference.take() {
            Some(deferred) => tokio::select! {
                biased;
                frame = frame_slot.recv() => (frame, true),
                _ = time::sleep_until(next_forced_keyframe) => (Some(deferred), false),
            },
            None => (frame_slot.recv().await, true),
        };
        let Some(frame) = frame else { return Ok(()) };
        if *shutdown.borrow() {
            return Ok(());
        }
        if newly_captured {
            record_metrics(metrics, |snapshot| snapshot.frames_taken_for_encode += 1);
        }
        let mut config = *updates.borrow();
        force_next_keyframe |= last_dimensions != Some((config.width, config.height));
        let repair_pending = force_next_keyframe || keyframe_requested.load(Ordering::Acquire);
        let waiting_for_cooldown = repair_pending && TokioInstant::now() < next_forced_keyframe;
        if waiting_for_cooldown {
            // Retain one capture so even an idle source can send the delayed keyframe.
            deferred_reference = Some(frame.clone());
        }
        let force_keyframe = if repair_pending && !waiting_for_cooldown {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(shutdown) => return Ok(()),
                ready = lane.reference_ready() => ready,
            }
        } else {
            false
        };
        if force_next_keyframe && !force_keyframe {
            // Keep the encoder's reference state until a replacement can be sent.
            record_metrics(metrics, |snapshot| {
                snapshot.frames_waiting_for_reference += 1
            });
            continue;
        }
        if !frame.has_new_damage && !force_keyframe {
            if !waiting_for_cooldown {
                record_metrics(metrics, |snapshot| snapshot.unchanged_frames_skipped += 1);
            }
            continue;
        }
        if force_keyframe {
            next_forced_keyframe = TokioInstant::now() + MIN_FORCED_KEYFRAME_INTERVAL;
            config = *updates.borrow();
            keyframe_requested.swap(false, Ordering::Acquire);
        } else {
            let ready = tokio::select! {
                biased;
                _ = wait_for_shutdown(shutdown) => return Ok(()),
                ready = lane.delta_ready() => ready,
            };
            if !ready {
                // Skipping the capture keeps the encoder's previous reference usable.
                record_metrics(metrics, |snapshot| {
                    snapshot.frames_skipped_for_backpressure += 1;
                });
                continue;
            }
        }
        last_dimensions = Some((config.width, config.height));
        force_next_keyframe = false;
        let next_sequence = stream_sequence.wrapping_add(1);
        let capture_time_us = frame.capture_time_us;
        let capture_sequence = frame.capture_sequence;
        let encode_started = Instant::now();
        let frame = match scaler.scale(frame, config.width, config.height) {
            Ok(frame) => frame,
            Err(error) => {
                record_metrics(metrics, |snapshot| snapshot.encode_errors += 1);
                return Err(error);
            }
        };
        let request = EncodeRequest {
            stream_sequence: next_sequence,
            force_keyframe,
            target_bitrate: config.target_bitrate,
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
            // A rate-control skip leaves the previous reference intact. Retry an outstanding
            // keyframe request, but do not spend a wire sequence on an unencoded image.
            record_metrics(metrics, |snapshot| {
                snapshot.encoder_skips += 1;
                snapshot.total_encode_micros += encode_micros;
                snapshot.max_encode_micros = snapshot.max_encode_micros.max(encode_micros);
            });
            force_next_keyframe = force_keyframe;
            continue;
        }
        if force_keyframe && !encoded.keyframe {
            record_metrics(metrics, |snapshot| snapshot.encode_errors += 1);
            return Err(MediaError::EncoderContract(
                "forced keyframe request was not honored",
            ));
        }

        stream_sequence = next_sequence;
        let sent_delta_during_cooldown = waiting_for_cooldown && !encoded.keyframe;
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

        // Only cancel an in-flight access unit when the session is closing. This
        // lets teardown proceed even if SCTP is still waiting for acknowledgments.
        let report = tokio::select! {
            biased;
            _ = wait_for_shutdown(shutdown) => return Ok(()),
            result = lane.write_access_unit(access_unit) => result,
        };
        match report {
            Ok(report) => {
                record_metrics(metrics, |snapshot| {
                    snapshot.lane_writes += 1;
                    snapshot.max_buffered_bytes = snapshot
                        .max_buffered_bytes
                        .max(report.buffered_bytes as u64);
                    if report.status == LaneWriteStatus::Written {
                        snapshot.deltas_written_during_keyframe_cooldown +=
                            u64::from(sent_delta_during_cooldown);
                        snapshot.bytes_written += report.wire_bytes as u64;
                        snapshot.next_video_sequence = Some(stream_sequence.wrapping_add(1));
                    } else {
                        snapshot.access_units_dropped_before_write += 1;
                    }
                });
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
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use nanoctl_core::video::VideoCodec;
    use tokio::sync::{Notify, mpsc};

    use super::MediaPipeline;
    use crate::types::{
        EncodeRequest, EncodedAccessUnit, EncodedBitstream, LaneWriteReport, LaneWriteStatus,
        LaneWriter, MediaError, MediaPipelineConfig, PixelFormat, RawFrame, VideoEncoder,
        VideoSource,
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
        delta_ready: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LaneWriter for CollectLane {
        async fn delta_ready(&mut self) -> bool {
            self.delta_ready.load(AtomicOrdering::SeqCst)
        }

        async fn write_access_unit(
            &mut self,
            access_unit: EncodedAccessUnit,
        ) -> Result<LaneWriteReport, MediaError> {
            let wire_bytes = if self.status == LaneWriteStatus::Written {
                access_unit.payload.len()
            } else {
                0
            };
            self.units
                .lock()
                .expect("collected units poisoned")
                .push(access_unit);
            self.notify.notify_one();
            Ok(LaneWriteReport {
                status: self.status,
                buffered_bytes: 0,
                wire_bytes,
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
                delta_ready: Arc::new(AtomicBool::new(true)),
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
        tokio::time::timeout(Duration::from_millis(250), wait_for_len(&units, &notify, 3))
            .await
            .expect("a usable delta must not wait for the keyframe cooldown");
        let mut unchanged = frame(444);
        unchanged.has_new_damage = false;
        frames_tx.send(unchanged).await.unwrap();
        wait_for_len(&units, &notify, 4).await;

        let observed: Vec<_> = units
            .lock()
            .expect("collected units poisoned")
            .iter()
            .map(|unit| (unit.stream_sequence, unit.keyframe, unit.capture_time_us))
            .collect();
        assert_eq!(
            observed,
            [
                (1, true, 111),
                (2, false, 222),
                (3, false, 333),
                (4, true, 444)
            ]
        );

        let report = pipeline.stop().await;
        assert!(report.capture_result.is_ok());
        assert!(report.encode_result.is_ok());
        assert_eq!(report.snapshot.frames_captured, 4);
        assert_eq!(report.snapshot.frames_taken_for_encode, 4);
        assert_eq!(report.snapshot.access_units_encoded, 4);
        assert_eq!(report.snapshot.keyframes_encoded, 2);
        assert_eq!(report.snapshot.deltas_written_during_keyframe_cooldown, 1);
    }

    #[tokio::test]
    async fn delta_backpressure_preserves_sequence_and_allows_reference_repair() {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let delta_ready = Arc::clone(&collecting.delta_ready);
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
        delta_ready.store(false, AtomicOrdering::SeqCst);
        frames_tx.send(frame(222)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while pipeline.snapshot().frames_skipped_for_backpressure == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("capture was not skipped during delta backpressure");

        delta_ready.store(true, AtomicOrdering::SeqCst);
        frames_tx.send(frame(333)).await.unwrap();
        wait_for_len(&units, &notify, 2).await;
        delta_ready.store(false, AtomicOrdering::SeqCst);
        pipeline.request_keyframe();
        let mut unchanged = frame(444);
        unchanged.has_new_damage = false;
        frames_tx.send(unchanged).await.unwrap();
        wait_for_len(&units, &notify, 3).await;

        let observed: Vec<_> = units
            .lock()
            .expect("collected units poisoned")
            .iter()
            .map(|unit| (unit.stream_sequence, unit.keyframe, unit.capture_time_us))
            .collect();
        assert_eq!(observed, [(1, true, 111), (2, false, 333), (3, true, 444)]);
        let report = pipeline.stop().await;
        assert!(report.encode_result.is_ok());
        assert_eq!(report.snapshot.frames_skipped_for_backpressure, 1);
        assert_eq!(report.snapshot.access_units_encoded, 3);
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
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            units.lock().unwrap().len(),
            1,
            "backpressure must not immediately send another IDR"
        );
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

    struct SkippingEncoder {
        attempts: usize,
        skip_at: usize,
    }

    #[async_trait]
    impl VideoEncoder for SkippingEncoder {
        async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
            let skip = self.attempts == self.skip_at;
            self.attempts += 1;
            if skip {
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
    async fn encoder_skip_retries_a_requested_keyframe_and_preserves_a_delta_chain() {
        for skip_at in [0, 1] {
            check_encoder_skip(skip_at).await;
        }
    }

    async fn check_encoder_skip(skip_at: usize) {
        let (frames_tx, frames_rx) = mpsc::channel(4);
        let (collecting, units, notify) = lane(LaneWriteStatus::Written);
        let pipeline = MediaPipeline::start(
            config(),
            Box::new(ChannelSource {
                receiver: frames_rx,
            }),
            Box::new(SkippingEncoder {
                attempts: 0,
                skip_at,
            }),
            Box::new(collecting),
        )
        .unwrap();

        let mut expected_frames = 0;
        for attempt in 0..3 {
            frames_tx
                .send(frame(111 * (attempt as u64 + 1)))
                .await
                .unwrap();
            if attempt == skip_at {
                tokio::time::timeout(Duration::from_secs(1), async {
                    while pipeline.snapshot().encoder_skips == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                expected_frames += 1;
                wait_for_len(&units, &notify, expected_frames).await;
            }
        }

        let written_frames = {
            let written = units.lock().expect("collected units poisoned");
            written
                .iter()
                .map(|unit| (unit.stream_sequence, unit.keyframe))
                .collect::<Vec<_>>()
        };
        assert_eq!(written_frames, [(1, true), (2, false)]);

        let report = pipeline.stop().await;
        assert_eq!(report.snapshot.encoder_skips, 1);
        assert_eq!(report.snapshot.access_units_encoded, 2);
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
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("unchanged frame was not processed");
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
