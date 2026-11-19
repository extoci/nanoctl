//! Cross-platform capture and baseline H.264 encoding.
//!
//! Platform release builds enable the `media` feature. The queue between this producer and WebRTC
//! is deliberately capacity one: a remote desktop should drop an obsolete frame under load rather
//! than preserve it and grow latency.

use anyhow::{Context, Result, anyhow, bail};
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode,
    UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer, YUVSource};
use xcap::Monitor;

use std::future::Future;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU16, Ordering},
};
use std::time::{Duration, Instant};
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::config::QualityConfig;
use crate::config::{EncoderPreference, LatencyMode};

const VIDEO_DELIVERY_FAILURE_GRACE: Duration = Duration::from_secs(15);
const VIDEO_SAMPLE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const CAPTURE_FRAME_STALE_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct VideoDeliveryUnavailable;

impl std::fmt::Display for VideoDeliveryUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("video sample delivery remained unavailable")
    }
}

impl std::error::Error for VideoDeliveryUnavailable {}

pub(crate) fn is_video_delivery_unavailable(error: &anyhow::Error) -> bool {
    error.is::<VideoDeliveryUnavailable>()
}

fn delivery_failure_grace_elapsed(
    failed: bool,
    failed_since: &mut Option<Instant>,
    now: Instant,
) -> bool {
    if !failed {
        *failed_since = None;
        return false;
    }
    let started = failed_since.get_or_insert(now);
    now.duration_since(*started) >= VIDEO_DELIVERY_FAILURE_GRACE
}

async fn await_video_delivery<F>(delivery: F, timeout: Duration) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    tokio::time::timeout(timeout, delivery)
        .await
        .context("video sample delivery timed out")?
}

#[derive(Default)]
struct LatestFrame {
    slot: Mutex<Option<CapturedFrame>>,
    last: Mutex<Option<CapturedFrame>>,
    ready: Condvar,
    stopped: AtomicBool,
}

#[derive(Clone)]
struct CapturedFrame {
    frame: xcap::Frame,
    captured_at: Instant,
}

impl LatestFrame {
    fn replace(&self, frame: xcap::Frame) -> bool {
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        let captured = CapturedFrame {
            frame,
            captured_at: Instant::now(),
        };
        if let Ok(mut last) = self.last.lock() {
            *last = Some(captured.clone());
        }
        *slot = Some(captured);
        self.ready.notify_one();
        true
    }

    fn take(&self, timeout: Duration) -> Result<CapturedFrame> {
        let slot = self
            .slot
            .lock()
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        if slot.is_none()
            && let Some(last) = self
                .last
                .lock()
                .map_err(|_| anyhow!("display capture queue was poisoned"))?
                .clone()
        {
            return Ok(last);
        }
        let (mut slot, timeout) = self
            .ready
            .wait_timeout_while(slot, timeout, |frame| frame.is_none())
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        if timeout.timed_out() && slot.is_none() {
            return self
                .last
                .lock()
                .map_err(|_| anyhow!("display capture queue was poisoned"))?
                .clone()
                .context("display capture stopped without a frame");
        }
        slot.take()
            .context("display capture stopped without a frame")
    }

    fn take_until_stopped(
        &self,
        timeout: Duration,
        stopped: &AtomicBool,
    ) -> Result<Option<CapturedFrame>> {
        let deadline = Instant::now() + timeout;
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        loop {
            if let Some(frame) = slot.take() {
                return Ok(Some(frame));
            }
            if stopped.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some(last) = self
                .last
                .lock()
                .map_err(|_| anyhow!("display capture queue was poisoned"))?
                .clone()
            {
                return Ok(Some(last));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self
                    .last
                    .lock()
                    .map_err(|_| anyhow!("display capture queue was poisoned"))?
                    .clone()
                    .map(Some)
                    .context("display capture stopped without a frame");
            }
            let (next_slot, _) = self
                .ready
                .wait_timeout(slot, remaining.min(Duration::from_millis(100)))
                .map_err(|_| anyhow!("display capture queue was poisoned"))?;
            slot = next_slot;
        }
    }
}

pub struct CaptureEncoder {
    // Keeping the recorder alive preserves one native capture session instead of rebuilding the
    // OS capture pipeline for every frame.
    recorder: Option<xcap::VideoRecorder>,
    forwarder: Option<std::thread::JoinHandle<()>>,
    latest_frame: Arc<LatestFrame>,
    display_id: String,
    encoder: EncoderBackend,
}

pub(crate) trait FrameEncoder {
    fn force_keyframe(&mut self);
    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()>;
    fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame>;
}

#[derive(Clone, Copy)]
struct EncoderSettings {
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
}

type OpenEncoder = fn(EncoderSettings, (u32, u32)) -> Result<Box<dyn FrameEncoder>>;

struct HardwareBackend {
    id: &'static str,
    name: &'static str,
    open: OpenEncoder,
}

macro_rules! hardware_backend {
    ($id:literal, $name:literal, $encoder:path) => {
        HardwareBackend {
            id: $id,
            name: $name,
            open: |settings, dimensions| {
                Ok(Box::new(<$encoder>::new(
                    settings.bitrate_kbps,
                    settings.max_fps,
                    settings.latency_mode,
                    dimensions,
                )?))
            },
        }
    };
}

const HARDWARE_BACKENDS: &[HardwareBackend] = &[
    #[cfg(target_os = "linux")]
    hardware_backend!("vaapi", "VA-API", crate::linux_encoder::LinuxEncoder),
    #[cfg(target_os = "linux")]
    hardware_backend!("nvenc", "NVENC", crate::nvenc_encoder::NvencEncoder),
    #[cfg(target_os = "macos")]
    hardware_backend!(
        "videotoolbox",
        "VideoToolbox",
        crate::macos_encoder::MacOsEncoder
    ),
    #[cfg(target_os = "windows")]
    hardware_backend!(
        "media-foundation",
        "Media Foundation hardware MFT",
        crate::windows_encoder::WindowsEncoder
    ),
];

struct ActiveEncoder {
    hardware: Option<&'static HardwareBackend>,
    encoder: Box<dyn FrameEncoder>,
    needs_initial_idr: bool,
}

struct EncoderBackend {
    active: Option<ActiveEncoder>,
    remaining_hardware: &'static [HardwareBackend],
    preference: EncoderPreference,
    settings: EncoderSettings,
    dimensions: (u32, u32),
}

struct SoftwareEncoder {
    encoder: Encoder,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
    dimensions: Option<(u32, u32)>,
    rgb: Vec<u8>,
    yuv: Option<YUVBuffer>,
}

impl CaptureEncoder {
    pub fn primary(
        max_bitrate_kbps: u32,
        max_fps: u16,
        max_width: u32,
        max_height: u32,
        latency_mode: LatencyMode,
        encoder_preference: EncoderPreference,
    ) -> Result<Self> {
        let monitors = Monitor::all().context("screen capture is unavailable")?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .cloned()
            .context("no display is available")?;
        let (initial_width, initial_height) = fit_dimensions(
            monitor
                .width()
                .context("cannot read primary display width")?,
            monitor
                .height()
                .context("cannot read primary display height")?,
            max_width,
            max_height,
        );
        let encoder = EncoderBackend::new(
            encoder_preference,
            max_bitrate_kbps,
            max_fps,
            latency_mode,
            (initial_width, initial_height),
        )?;
        let display_id = monitor
            .id()
            .context("cannot read primary display identifier")?
            .to_string();
        let (recorder, latest_frame, forwarder) = start_capture(&monitor)?;
        Ok(Self {
            recorder: Some(recorder),
            forwarder: Some(forwarder),
            latest_frame,
            display_id,
            encoder,
        })
    }

    pub fn select_display(&mut self, display_id: &str) -> Result<()> {
        if display_id.len() > 128 {
            bail!("display identifier is too long");
        }
        let monitor = Monitor::all()
            .context("cannot enumerate displays for capture")?
            .into_iter()
            .find(|monitor| monitor.id().is_ok_and(|id| id.to_string() == display_id))
            .context("selected display is unavailable")?;
        // Start the replacement while the current recorder is still valid. If the new display
        // cannot be opened, the caller keeps the current stream instead of losing video merely
        // because a display-selection request was invalid or temporarily unavailable.
        let (recorder, latest_frame, forwarder) = start_capture(&monitor)?;
        self.latest_frame.stopped.store(true, Ordering::Release);
        if let Some(old_recorder) = self.recorder.take() {
            let _ = old_recorder.stop();
            drop(old_recorder);
        }
        if let Some(old_forwarder) = self.forwarder.take() {
            let _ = old_forwarder.join();
        }
        self.recorder = Some(recorder);
        self.forwarder = Some(forwarder);
        self.latest_frame = latest_frame;
        self.display_id = display_id.to_owned();
        self.encoder.force_keyframe();
        Ok(())
    }

    fn current_display_id(&self) -> &str {
        &self.display_id
    }

    pub fn apply_bitrate_estimate(
        &mut self,
        estimate_kbps: u32,
        ceiling_kbps: u32,
    ) -> Result<bool> {
        self.encoder
            .apply_bitrate_estimate(estimate_kbps, ceiling_kbps)
    }

    fn next_access_unit_with_timings(
        &mut self,
        max_width: u32,
        max_height: u32,
    ) -> Result<(EncodedFrame, TimingSample)> {
        let captured = self.latest_frame.take(Duration::from_secs(5))?;
        self.encode_captured_frame(captured, max_width, max_height)
    }

    fn encode_captured_frame(
        &mut self,
        captured: CapturedFrame,
        max_width: u32,
        max_height: u32,
    ) -> Result<(EncodedFrame, TimingSample)> {
        let CapturedFrame { frame, captured_at } = captured;
        if captured_at.elapsed() > CAPTURE_FRAME_STALE_GRACE {
            bail!("display capture stopped producing fresh frames");
        }
        let source = xcap::image::RgbaImage::from_raw(frame.width, frame.height, frame.raw)
            .context("display capture returned an invalid RGBA frame")?;
        let (source_width, source_height) = source.dimensions();
        let (target_width, target_height) =
            fit_dimensions(source_width, source_height, max_width, max_height);
        let image = if target_width == source_width && target_height == source_height {
            source
        } else {
            xcap::image::imageops::resize(
                &source,
                target_width,
                target_height,
                xcap::image::imageops::FilterType::Triangle,
            )
        };
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            bail!("captured display dimensions must be non-zero and even");
        }
        let encode_started = Instant::now();
        let encoded = self.encoder.encode(&image)?;
        let encoded_at = Instant::now();
        Ok((
            encoded,
            TimingSample {
                capture_to_encode: encoded_at.saturating_duration_since(captured_at),
                encode: encoded_at.saturating_duration_since(encode_started),
            },
        ))
    }

    fn next_access_unit_until_stopped(
        &mut self,
        max_width: u32,
        max_height: u32,
        stopped: &AtomicBool,
    ) -> Result<Option<EncodedFrame>> {
        let Some(frame) = self
            .latest_frame
            .take_until_stopped(Duration::from_secs(5), stopped)?
        else {
            return Ok(None);
        };
        if stopped.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(
            self.encode_captured_frame(frame, max_width, max_height)
                .map(|(encoded, _timing)| encoded)?,
        ))
    }
}

impl EncoderBackend {
    fn new(
        preference: EncoderPreference,
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        probe_dimensions: (u32, u32),
    ) -> Result<Self> {
        Self::with_hardware(
            preference,
            EncoderSettings {
                bitrate_kbps,
                max_fps,
                latency_mode,
            },
            probe_dimensions,
            HARDWARE_BACKENDS,
        )
    }

    fn with_hardware(
        preference: EncoderPreference,
        settings: EncoderSettings,
        dimensions: (u32, u32),
        hardware: &'static [HardwareBackend],
    ) -> Result<Self> {
        let mut backend = Self {
            active: None,
            remaining_hardware: if matches!(preference, EncoderPreference::Software) {
                &[]
            } else {
                hardware
            },
            preference,
            settings,
            dimensions,
        };
        backend.activate_next(None)?;
        Ok(backend)
    }

    fn activate_next(&mut self, mut last_error: Option<anyhow::Error>) -> Result<()> {
        // Some drivers support only one active hardware session.
        self.active.take();
        while let Some((hardware, remaining)) = self.remaining_hardware.split_first() {
            self.remaining_hardware = remaining;
            match (hardware.open)(self.settings, self.dimensions) {
                Ok(encoder) => {
                    self.active = Some(ActiveEncoder {
                        hardware: Some(hardware),
                        encoder,
                        needs_initial_idr: true,
                    });
                    return Ok(());
                }
                Err(error) => {
                    tracing::debug!(backend = hardware.name, error = %error, "hardware encoder unavailable");
                    last_error = Some(error.context(hardware.name));
                }
            }
        }
        if matches!(self.preference, EncoderPreference::Hardware) {
            return Err(
                last_error.unwrap_or_else(|| anyhow!("no native hardware encoder was compiled"))
            )
            .context("hardware H.264 was required but no usable backend remains");
        }
        if let Some(error) = last_error {
            tracing::debug!(%error, "no usable hardware encoder; using OpenH264");
        }
        self.active = Some(ActiveEncoder {
            hardware: None,
            encoder: Box::new(SoftwareEncoder::new(
                self.settings.bitrate_kbps,
                self.settings.max_fps,
                self.settings.latency_mode,
            )?),
            needs_initial_idr: true,
        });
        Ok(())
    }

    fn force_keyframe(&mut self) {
        if let Some(active) = &mut self.active {
            active.encoder.force_keyframe();
        }
    }

    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        let Some(target_kbps) = bitrate_target(
            self.settings.bitrate_kbps,
            estimate_kbps,
            250.min(ceiling_kbps),
            ceiling_kbps,
        ) else {
            return Ok(false);
        };
        let active = self.active.as_mut().context("no active H.264 encoder")?;
        self.settings.bitrate_kbps = target_kbps;
        if let Err(error) = active.encoder.set_bitrate(target_kbps) {
            let Some(hardware) = active.hardware else {
                return Err(error);
            };
            tracing::warn!(backend = hardware.name, %error, "hardware bitrate change failed; switching H.264 backend");
            self.activate_next(Some(error.context(hardware.name)))?;
        }
        Ok(true)
    }

    fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        if self.dimensions != image.dimensions()
            && let Some(active) = &mut self.active
        {
            active.needs_initial_idr = true;
        }
        self.dimensions = image.dimensions();
        loop {
            let active = self.active.as_mut().context("no active H.264 encoder")?;
            let encoded = active.encoder.encode(image).and_then(|frame| {
                if active.needs_initial_idr {
                    let types = annex_b_nal_types(&frame.bytes)?;
                    if ![5, 7, 8].iter().all(|nal| types.contains(nal)) {
                        bail!("first H.264 access unit did not contain IDR, SPS and PPS");
                    }
                    active.needs_initial_idr = false;
                }
                Ok(frame)
            });
            match encoded {
                Ok(frame) => return Ok(frame),
                Err(error) => {
                    let Some(hardware) = active.hardware else {
                        return Err(error);
                    };
                    tracing::warn!(backend = hardware.name, %error, "hardware encode failed; switching H.264 backend");
                    self.activate_next(Some(error.context(hardware.name)))?;
                }
            }
        }
    }

    fn selected_hardware(&self) -> Option<&'static HardwareBackend> {
        self.active.as_ref().and_then(|active| active.hardware)
    }

    fn name(&self) -> &'static str {
        self.selected_hardware()
            .map_or("OpenH264", |hardware| hardware.name)
    }

    fn id(&self) -> &'static str {
        self.selected_hardware()
            .map_or("software-openh264", |hardware| hardware.id)
    }
}

impl SoftwareEncoder {
    fn new(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Self> {
        Ok(Self {
            encoder: create_encoder(bitrate_kbps, max_fps, latency_mode)?,
            bitrate_kbps,
            max_fps,
            latency_mode,
            dimensions: None,
            rgb: Vec::new(),
            yuv: None,
        })
    }
}

impl FrameEncoder for SoftwareEncoder {
    fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }

    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()> {
        self.encoder = create_encoder(target_kbps, self.max_fps, self.latency_mode)?;
        self.bitrate_kbps = target_kbps;
        Ok(())
    }

    fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        let dimensions = (image.width(), image.height());
        if self.dimensions != Some(dimensions) {
            // OpenH264 does not promise dynamic resolution changes. Recreate the software
            // encoder at a display handoff just as the hardware backends do, guaranteeing a
            // fresh parameter set and an IDR for the new frame size.
            self.encoder = create_encoder(self.bitrate_kbps, self.max_fps, self.latency_mode)?;
            self.yuv = None;
            self.dimensions = Some(dimensions);
        }
        let dimensions = (dimensions.0 as usize, dimensions.1 as usize);
        self.rgb.clear();
        self.rgb.reserve(dimensions.0 * dimensions.1 * 3);
        for pixel in image.as_raw().chunks_exact(4) {
            self.rgb.extend_from_slice(&pixel[..3]);
        }
        let source = RgbSliceU8::new(&self.rgb, dimensions);
        let yuv = self
            .yuv
            .get_or_insert_with(|| YUVBuffer::new(dimensions.0, dimensions.1));
        if yuv.dimensions() != dimensions {
            *yuv = YUVBuffer::new(dimensions.0, dimensions.1);
        }
        yuv.read_rgb8(source);
        let stream = self.encoder.encode(yuv).context("H.264 encode failed")?;
        Ok(EncodedFrame {
            bytes: stream.to_vec(),
            width: image.width(),
            height: image.height(),
        })
    }
}

fn start_capture(
    monitor: &Monitor,
) -> Result<(
    xcap::VideoRecorder,
    Arc<LatestFrame>,
    std::thread::JoinHandle<()>,
)> {
    let (recorder, native_frames) = monitor
        .video_recorder()
        .context("continuous display capture is unavailable")?;
    let latest_frame = Arc::new(LatestFrame::default());
    let forwarding_slot = Arc::downgrade(&latest_frame);
    let forwarder = std::thread::Builder::new()
        .name("nanoctl-capture".to_owned())
        .spawn(move || {
            while let Some(latest_frame) = forwarding_slot.upgrade() {
                if latest_frame.stopped.load(Ordering::Acquire) {
                    break;
                }
                let frame = match native_frames.recv_timeout(Duration::from_millis(100)) {
                    Ok(frame) => frame,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
                // Replacement, rather than append, makes this queue exactly one frame deep.
                if !latest_frame.replace(frame) {
                    break;
                }
            }
        })
        .context("capture forwarding thread could not start")?;
    if let Err(error) = recorder.start() {
        latest_frame.stopped.store(true, Ordering::Release);
        let _ = recorder.stop();
        let _ = forwarder.join();
        return Err(error).context("continuous display capture could not start");
    }
    Ok((recorder, latest_frame, forwarder))
}

fn create_encoder(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Encoder> {
    let (complexity, skip_frames) = match latency_mode {
        LatencyMode::Responsiveness => (Complexity::Low, true),
        LatencyMode::Balanced => (Complexity::Medium, true),
        LatencyMode::Quality => (Complexity::High, false),
    };
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate_kbps.saturating_mul(1_000)))
        .max_frame_rate(FrameRate::from_hz(max_fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(u32::from(max_fps) * 2))
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .complexity(complexity)
        .skip_frames(skip_frames);
    Encoder::with_api_config(OpenH264API::from_source(), config)
        .context("H.264 encoder is unavailable")
}

pub fn probe_encoder(preference: EncoderPreference) -> Result<(&'static str, &'static str)> {
    let encoder = EncoderBackend::new(preference, 1_000, 30, LatencyMode::Balanced, (1_280, 720))?;
    Ok((encoder.id(), encoder.name()))
}

pub(crate) fn bitrate_target(current: u32, estimate: u32, floor: u32, ceiling: u32) -> Option<u32> {
    let target = estimate.clamp(floor, ceiling);
    let target = u64::from(target);
    let current = u64::from(current);
    if target * 100 <= current * 80 || target * 100 >= current * 125 {
        Some(target as u32)
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn avcc_to_annex_b(
    access_unit: &[u8],
    nal_length_size: usize,
    parameter_sets: &[&[u8]],
) -> Result<Vec<u8>> {
    if !(1..=4).contains(&nal_length_size) {
        bail!("AVCC NAL length field must contain between 1 and 4 bytes");
    }
    let parameter_bytes = parameter_sets
        .iter()
        .try_fold(0_usize, |total, parameter_set| {
            if parameter_set.is_empty() || parameter_set.len() > 64 * 1024 {
                return None;
            }
            total.checked_add(4)?.checked_add(parameter_set.len())
        });
    let parameter_bytes = parameter_bytes.context("H.264 parameter sets are invalid")?;
    let capacity = parameter_bytes
        .checked_add(access_unit.len())
        .context("H.264 access unit is too large")?;
    let mut annex_b = Vec::with_capacity(capacity);
    for parameter_set in parameter_sets {
        annex_b.extend_from_slice(&[0, 0, 0, 1]);
        annex_b.extend_from_slice(parameter_set);
    }
    let mut offset = 0_usize;
    while offset < access_unit.len() {
        let length_end = offset
            .checked_add(nal_length_size)
            .context("AVCC offset overflow")?;
        let encoded_length = access_unit
            .get(offset..length_end)
            .context("AVCC access unit ends inside a NAL length field")?;
        let nal_length = encoded_length
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        if nal_length == 0 {
            bail!("AVCC access unit contains an empty NAL unit");
        }
        let nal_end = length_end
            .checked_add(nal_length)
            .context("AVCC NAL length overflow")?;
        let nal = access_unit
            .get(length_end..nal_end)
            .context("AVCC NAL length exceeds the access unit")?;
        annex_b.extend_from_slice(&[0, 0, 0, 1]);
        annex_b.extend_from_slice(nal);
        offset = nal_end;
    }
    if offset == 0 {
        bail!("AVCC access unit is empty");
    }
    Ok(annex_b)
}

impl Drop for CaptureEncoder {
    fn drop(&mut self) {
        self.latest_frame.stopped.store(true, Ordering::Release);
        if let Some(recorder) = self.recorder.take() {
            let _ = recorder.stop();
            drop(recorder);
        }
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
    }
}

pub struct EncodedFrame {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A bounded handoff that always retains the newest encoded frame. A full FIFO is the wrong
/// shape for remote desktop video: keeping an obsolete frame adds latency and can discard the
/// IDR requested for recovery.
struct EncodedFrameSlot {
    frame: Mutex<Option<EncodedFrame>>,
    ready: tokio::sync::Notify,
    closed: AtomicBool,
    dropped: AtomicU16,
}

impl EncodedFrameSlot {
    fn new() -> Self {
        Self {
            frame: Mutex::new(None),
            ready: tokio::sync::Notify::new(),
            closed: AtomicBool::new(false),
            dropped: AtomicU16::new(0),
        }
    }

    fn replace(&self, frame: EncodedFrame) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let Ok(mut slot) = self.frame.lock() else {
            return false;
        };
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        // Never overwrite a recovery keyframe with a delta frame. The consumer may be briefly
        // backpressured while Chromium requests an IDR; dropping that IDR would leave the peer
        // waiting for the next periodic keyframe and turn a transient loss into visible outage.
        let incoming_is_keyframe = is_idr(&frame.bytes);
        if slot
            .as_ref()
            .is_some_and(|current| is_idr(&current.bytes) && !incoming_is_keyframe)
        {
            self.record_drop();
            return true;
        }
        if slot.is_some() {
            self.record_drop();
        }
        *slot = Some(frame);
        self.ready.notify_one();
        true
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_waiters();
    }

    fn record_drop(&self) {
        let _ = self
            .dropped
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_add(1))
            });
    }

    async fn take(&self) -> Option<(EncodedFrame, u16)> {
        loop {
            // Register the notification before checking the condition. Notify is not a counting
            // semaphore, so creating the future afterwards has a close/notify lost-wakeup race.
            let notified = self.ready.notified();
            match self.frame.lock() {
                Ok(mut slot) => {
                    if let Some(frame) = slot.take() {
                        let dropped = self.dropped.swap(0, Ordering::AcqRel);
                        return Some((frame, dropped));
                    }
                }
                Err(_) => {
                    self.closed.store(true, Ordering::Release);
                    return None;
                }
            }
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            notified.await;
        }
    }
}

fn is_idr(bytes: &[u8]) -> bool {
    let mut offset = 0_usize;
    while offset + 3 < bytes.len() {
        let start_length = if bytes[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        let Some(&header) = bytes.get(offset + start_length) else {
            return false;
        };
        match header & 0x1f {
            5 => return true,
            1..=4 => return false,
            _ => offset += start_length + 1,
        }
    }
    false
}

struct TimingSample {
    capture_to_encode: Duration,
    encode: Duration,
}

#[derive(Default)]
struct TimingSamples {
    capture_to_encode_micros: Vec<u64>,
    encode_micros: Vec<u64>,
}

const MAX_TIMING_SAMPLES: usize = 524_288;

impl TimingSamples {
    fn observe(&mut self, sample: TimingSample) {
        if self.capture_to_encode_micros.len() < MAX_TIMING_SAMPLES {
            self.capture_to_encode_micros.push(
                sample
                    .capture_to_encode
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            );
            self.encode_micros
                .push(sample.encode.as_micros().min(u128::from(u64::MAX)) as u64);
        }
    }

    fn percentiles(&mut self) -> TimingPercentiles {
        self.capture_to_encode_micros.sort_unstable();
        self.encode_micros.sort_unstable();
        TimingPercentiles {
            capture_to_encode_p50_milliseconds: percentile_milliseconds(
                &self.capture_to_encode_micros,
                0.50,
            ),
            capture_to_encode_p95_milliseconds: percentile_milliseconds(
                &self.capture_to_encode_micros,
                0.95,
            ),
            encode_p50_milliseconds: percentile_milliseconds(&self.encode_micros, 0.50),
            encode_p95_milliseconds: percentile_milliseconds(&self.encode_micros, 0.95),
        }
    }
}

struct TimingPercentiles {
    capture_to_encode_p50_milliseconds: f64,
    capture_to_encode_p95_milliseconds: f64,
    encode_p50_milliseconds: f64,
    encode_p95_milliseconds: f64,
}

fn percentile_milliseconds(samples: &[u64], percentile: f64) -> f64 {
    let Some(last) = samples.len().checked_sub(1) else {
        return 0.0;
    };
    let index = ((last as f64) * percentile).round() as usize;
    samples[index.min(last)] as f64 / 1_000.0
}

#[derive(Debug, serde::Serialize)]
pub struct MediaSmokeReport {
    pub schema_version: u8,
    pub passed: bool,
    pub agent_version: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub started_unix_seconds: u64,
    pub backend: &'static str,
    pub requested_seconds: u64,
    pub requested_fps: u16,
    pub requested_max_bitrate_kbps: u32,
    pub requested_max_width: u32,
    pub requested_max_height: u32,
    pub elapsed_milliseconds: u128,
    pub frames: u64,
    pub frames_per_second: f64,
    pub encoded_bytes: u64,
    pub average_bitrate_kbps: f64,
    /// Time from native capture delivery to the completed encoded access unit.
    pub capture_to_encode_p50_milliseconds: f64,
    pub capture_to_encode_p95_milliseconds: f64,
    pub encode_p50_milliseconds: f64,
    pub encode_p95_milliseconds: f64,
    pub width: u32,
    pub height: u32,
    pub idr_frames: u64,
    pub sps_units: u64,
    pub pps_units: u64,
}

impl MediaSmokeReport {
    pub fn print(&self) {
        println!(
            "media-smoke={} version={} target={}-{} backend={} frames={} fps={:.2} bitrate_kbps={:.2} \
             capture_to_encode_p95_ms={:.2} encode_p95_ms={:.2} dimensions={}x{} idr={} sps={} pps={}",
            if self.passed { "pass" } else { "fail" },
            self.agent_version,
            self.target_os,
            self.target_arch,
            self.backend,
            self.frames,
            self.frames_per_second,
            self.average_bitrate_kbps,
            self.capture_to_encode_p95_milliseconds,
            self.encode_p95_milliseconds,
            self.width,
            self.height,
            self.idr_frames,
            self.sps_units,
            self.pps_units
        );
    }
}

pub fn run_smoke(quality: &QualityConfig, seconds: u64) -> Result<MediaSmokeReport> {
    if !(1..=3_600).contains(&seconds) {
        bail!("media smoke duration must be between 1 and 3600 seconds");
    }
    let mut encoder = CaptureEncoder::primary(
        quality.max_bitrate_kbps,
        quality.max_fps,
        quality.max_width,
        quality.max_height,
        quality.latency_mode,
        quality.encoder,
    )?;
    let started_unix_seconds = crate::update::unix_time_now()?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(quality.max_fps));
    let mut next_frame = started;
    let mut frames = 0_u64;
    let mut encoded_bytes = 0_u64;
    let mut idr_frames = 0_u64;
    let mut sps_units = 0_u64;
    let mut pps_units = 0_u64;
    let mut dimensions = (0_u32, 0_u32);
    let mut timing_samples = TimingSamples::default();
    while Instant::now() < deadline {
        let now = Instant::now();
        if now < next_frame {
            std::thread::sleep(next_frame - now);
        }
        next_frame = Instant::now() + frame_interval;
        let (frame, timing) =
            encoder.next_access_unit_with_timings(quality.max_width, quality.max_height)?;
        timing_samples.observe(timing);
        if frame.bytes.is_empty() {
            continue;
        }
        let nal_types = annex_b_nal_types(&frame.bytes)?;
        if nal_types.contains(&5) {
            idr_frames += 1;
        }
        sps_units += nal_types.iter().filter(|&&kind| kind == 7).count() as u64;
        pps_units += nal_types.iter().filter(|&&kind| kind == 8).count() as u64;
        encoded_bytes = encoded_bytes
            .checked_add(frame.bytes.len() as u64)
            .context("encoded byte counter overflow")?;
        frames += 1;
        if dimensions == (0, 0) {
            dimensions = (frame.width, frame.height);
        }
    }
    let elapsed = started.elapsed();
    let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
    let frames_per_second = frames as f64 / elapsed_seconds;
    let average_bitrate_kbps = encoded_bytes as f64 * 8.0 / elapsed_seconds / 1_000.0;
    let minimum_frames = (seconds as f64 * f64::from(quality.max_fps) * 0.75).floor() as u64;
    let passed = frames >= minimum_frames
        && encoded_bytes > 0
        && idr_frames > 0
        && sps_units > 0
        && pps_units > 0;
    // Auto mode may switch from a native encoder to OpenH264 after the first real frame. Report
    // the backend that completed the smoke test, not merely the one that initialized.
    let backend = encoder.encoder.name();
    let timing = timing_samples.percentiles();
    Ok(MediaSmokeReport {
        schema_version: 1,
        passed,
        agent_version: env!("CARGO_PKG_VERSION"),
        target_os: std::env::consts::OS,
        target_arch: std::env::consts::ARCH,
        started_unix_seconds,
        backend,
        requested_seconds: seconds,
        requested_fps: quality.max_fps,
        requested_max_bitrate_kbps: quality.max_bitrate_kbps,
        requested_max_width: quality.max_width,
        requested_max_height: quality.max_height,
        elapsed_milliseconds: elapsed.as_millis(),
        frames,
        frames_per_second,
        encoded_bytes,
        average_bitrate_kbps,
        capture_to_encode_p50_milliseconds: timing.capture_to_encode_p50_milliseconds,
        capture_to_encode_p95_milliseconds: timing.capture_to_encode_p95_milliseconds,
        encode_p50_milliseconds: timing.encode_p50_milliseconds,
        encode_p95_milliseconds: timing.encode_p95_milliseconds,
        width: dimensions.0,
        height: dimensions.1,
        idr_frames,
        sps_units,
        pps_units,
    })
}

pub(crate) fn annex_b_nal_types(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > 32 * 1024 * 1024 {
        bail!("H.264 access unit exceeds the size limit");
    }
    let mut types = Vec::new();
    let mut offset = 0_usize;
    while offset + 3 < bytes.len() {
        let start_length = if bytes[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        let header = offset + start_length;
        let byte = *bytes
            .get(header)
            .context("H.264 access unit ends after a start code")?;
        types.push(byte & 0x1f);
        offset = header + 1;
    }
    if types.is_empty() {
        bail!("H.264 access unit contains no Annex-B NAL units");
    }
    Ok(types)
}

#[derive(Clone, Copy)]
pub struct VideoQuality {
    pub max_bitrate_kbps: u32,
    pub max_fps: u16,
    pub max_width: u32,
    pub max_height: u32,
    pub latency_mode: LatencyMode,
    pub encoder_preference: EncoderPreference,
}

/// Owns both halves of the capture pipeline so shutdown can wait for the blocking worker to
/// release native capture and encoder resources before a replacement session starts.
pub(crate) struct VideoPipeline {
    task: tokio::task::JoinHandle<Result<()>>,
    stop: tokio::sync::watch::Sender<bool>,
    stopped: Arc<AtomicBool>,
}

impl VideoPipeline {
    pub(crate) async fn stop(self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.stop.send(true);
        // Do not abort this task. It owns a spawn_blocking capture/Media Foundation worker, and
        // Tokio cannot cancel a blocking thread after it has entered native code. Awaiting the
        // task is what makes close→reopen release the old native capture and encoder resources
        // before the next session starts.
        if let Err(error) = self.task.await {
            tracing::warn!(error = %error, "video pipeline task stopped unexpectedly");
        }
    }

    pub(crate) async fn finish(self) -> Result<()> {
        self.task
            .await
            .context("video pipeline task stopped unexpectedly")?
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    #[cfg(test)]
    fn test_worker(finished: Arc<AtomicBool>) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let worker_stopped = stopped.clone();
        let task = tokio::spawn(async move {
            let worker = tokio::task::spawn_blocking(move || {
                while !worker_stopped.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                finished.store(true, Ordering::Release);
            });
            let _ = stop_rx.changed().await;
            worker
                .await
                .context("test capture worker stopped unexpectedly")?;
            Ok(())
        });
        Self {
            task,
            stop,
            stopped,
        }
    }
}

pub fn spawn_video(
    track: Arc<TrackLocalStaticSample>,
    keyframe_requests: tokio::sync::watch::Receiver<u64>,
    bitrate_estimate_kbps: tokio::sync::watch::Receiver<u32>,
    display_selection: tokio::sync::watch::Receiver<String>,
    display_selection_feedback: tokio::sync::watch::Sender<String>,
    quality: VideoQuality,
) -> VideoPipeline {
    let stopped = Arc::new(AtomicBool::new(false));
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let worker_stopped = stopped.clone();
    let task = tokio::spawn(async move {
        let encoded_slot = Arc::new(EncodedFrameSlot::new());
        let (delivery_recovery, mut delivery_recovery_requests) =
            tokio::sync::watch::channel(0_u64);
        let producer_slot = encoded_slot.clone();
        let producer_stopped = worker_stopped.clone();
        let producer = tokio::task::spawn_blocking(move || -> Result<()> {
            let result = (|| -> Result<()> {
                let mut encoder = CaptureEncoder::primary(
                    quality.max_bitrate_kbps,
                    quality.max_fps,
                    quality.max_width,
                    quality.max_height,
                    quality.latency_mode,
                    quality.encoder_preference,
                )?;
                let mut keyframe_requests = keyframe_requests;
                let mut bitrate_estimate_kbps = bitrate_estimate_kbps;
                let mut display_selection = display_selection;
                let display_selection_feedback = display_selection_feedback;
                let frame_interval = Duration::from_secs_f64(1.0 / f64::from(quality.max_fps));
                let mut next_frame = Instant::now();
                loop {
                    if producer_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let now = Instant::now();
                    if now < next_frame {
                        std::thread::sleep(next_frame - now);
                    }
                    if producer_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    // Advance from the actual clock when capture/encode overruns. This prevents a
                    // burst of catch-up frames after a slow encode or a suspended machine.
                    next_frame = Instant::now() + frame_interval;
                    if keyframe_requests.has_changed().unwrap_or(false) {
                        keyframe_requests.borrow_and_update();
                        encoder.encoder.force_keyframe();
                    }
                    if delivery_recovery_requests.has_changed().unwrap_or(false) {
                        delivery_recovery_requests.borrow_and_update();
                        encoder.encoder.force_keyframe();
                    }
                    if bitrate_estimate_kbps.has_changed().unwrap_or(false) {
                        let estimate = *bitrate_estimate_kbps.borrow_and_update();
                        encoder.apply_bitrate_estimate(estimate, quality.max_bitrate_kbps)?;
                    }
                    if display_selection.has_changed().unwrap_or(false) {
                        let display_id = display_selection.borrow_and_update().clone();
                        if !display_id.is_empty()
                            && let Err(error) = encoder.select_display(&display_id)
                        {
                            tracing::warn!(
                                display_id,
                                error = %error,
                                "display switch rejected; retaining current capture"
                            );
                            // Input geometry is updated optimistically by the reliable control
                            // worker. If capture rejects the same request, publish the actual
                            // capture target so that worker rolls its geometry back. Do not
                            // overwrite a newer request that arrived while the switch ran.
                            if display_selection.borrow().as_str() == display_id.as_str() {
                                display_selection_feedback
                                    .send_replace(encoder.current_display_id().to_owned());
                            }
                        }
                    }
                    let Some(frame) = encoder.next_access_unit_until_stopped(
                        quality.max_width,
                        quality.max_height,
                        &producer_stopped,
                    )?
                    else {
                        break;
                    };
                    if frame.bytes.is_empty() {
                        continue;
                    }
                    if !producer_slot.replace(frame) {
                        break;
                    }
                }
                Ok(())
            })();
            producer_slot.close();
            result
        });
        let mut stop_rx = stop_rx;
        let duration = Duration::from_secs_f64(1.0 / f64::from(quality.max_fps));
        let mut delivery_interrupted_since: Option<Instant> = None;
        let mut delivery_error_count = 0_u64;
        let mut recovery_keyframe_requested = false;
        let mut write_result = Ok(());
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                frame = encoded_slot.take() => {
                    let Some((frame, _dropped)) = frame else { break };
                    // A replaced encoded frame is a local latency policy, not RTP packet loss.
                    // Passing that count as prev_dropped_packets corrupts packet sequencing.
                    let sample = Sample {
                        data: frame.bytes.into(),
                        duration,
                        ..Default::default()
                    };
                    // A broken transport must not hold capture/encoder teardown hostage forever.
                    // Cancelling the packetizer future is safe; the next successful delivery is
                    // followed by a forced recovery keyframe.
                    let result = await_video_delivery(
                        async {
                            track
                                .write_sample(&sample)
                                .await
                                .map_err(anyhow::Error::from)
                        },
                        VIDEO_SAMPLE_WRITE_TIMEOUT,
                    )
                    .await;
                    match result {
                        Ok(()) if delivery_interrupted_since.is_some() => {
                            let outage = delivery_interrupted_since
                                .map_or(Duration::ZERO, |started| started.elapsed());
                            delivery_failure_grace_elapsed(
                                false,
                                &mut delivery_interrupted_since,
                                Instant::now(),
                            );
                            delivery_recovery.send_modify(|generation| {
                                *generation = generation.wrapping_add(1);
                            });
                            tracing::info!(
                                outage_milliseconds = outage.as_millis(),
                                delivery_errors = delivery_error_count,
                                "video sample delivery recovered; keyframe requested"
                            );
                            delivery_error_count = 0;
                            recovery_keyframe_requested = false;
                        }
                        Ok(()) => {}
                        Err(error) => {
                            delivery_error_count = delivery_error_count.saturating_add(1);
                            if !recovery_keyframe_requested {
                                delivery_recovery.send_modify(|generation| {
                                    *generation = generation.wrapping_add(1);
                                });
                                recovery_keyframe_requested = true;
                            }
                            if delivery_interrupted_since.is_none() {
                                // RTP/SRTP writes can fail while ICE reconnects or a browser peer is
                                // closing. Capture and encoding remain healthy, so restarting native
                                // resources here only creates encoder contention and guarantees the
                                // same closed peer rejects the replacement pipeline too.
                                tracing::warn!(
                                    error = %error,
                                    "video sample delivery interrupted; retaining media pipeline"
                                );
                            }
                            if delivery_failure_grace_elapsed(
                                true,
                                &mut delivery_interrupted_since,
                                Instant::now(),
                            ) {
                                write_result = Err(anyhow!(VideoDeliveryUnavailable));
                                break;
                            }
                        }
                    }
                }
            }
        }
        worker_stopped.store(true, Ordering::Release);
        let producer_result = producer
            .await
            .context("video capture worker stopped unexpectedly")?;
        producer_result?;
        write_result?;
        Ok(())
    });
    VideoPipeline {
        task,
        stop,
        stopped,
    }
}

fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    let fitted_width = ((f64::from(width) * scale).floor() as u32).max(2) & !1;
    let fitted_height = ((f64::from(height) * scale).floor() as u32).max(2) & !1;
    (fitted_width, fitted_height)
}

#[cfg(test)]
mod tests {
    use super::{
        EncodedFrame, EncodedFrameSlot, EncoderBackend, EncoderSettings, FrameEncoder,
        HARDWARE_BACKENDS, HardwareBackend, LatestFrame, VIDEO_DELIVERY_FAILURE_GRACE,
        annex_b_nal_types, avcc_to_annex_b, await_video_delivery, bitrate_target,
        delivery_failure_grace_elapsed, fit_dimensions,
    };
    use crate::config::{EncoderPreference, LatencyMode};
    use std::time::{Duration, Instant};
    use xcap::image::{Rgba, RgbaImage};

    #[tokio::test]
    async fn stopping_a_video_pipeline_waits_for_the_blocking_capture_worker() {
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pipeline = super::VideoPipeline::test_worker(finished.clone());

        pipeline.stop().await;

        assert!(finished.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn transient_delivery_failure_does_not_restart_native_media() {
        let started = Instant::now();
        let mut failed_since = None;

        assert!(!delivery_failure_grace_elapsed(
            true,
            &mut failed_since,
            started,
        ));
        assert!(!delivery_failure_grace_elapsed(
            true,
            &mut failed_since,
            started + VIDEO_DELIVERY_FAILURE_GRACE - Duration::from_millis(1),
        ));
        assert!(!delivery_failure_grace_elapsed(
            false,
            &mut failed_since,
            started + VIDEO_DELIVERY_FAILURE_GRACE,
        ));
        assert!(failed_since.is_none());
        assert!(!delivery_failure_grace_elapsed(
            true,
            &mut failed_since,
            started + VIDEO_DELIVERY_FAILURE_GRACE,
        ));
    }

    #[test]
    fn permanent_delivery_failure_is_terminal_after_grace() {
        let started = Instant::now();
        let mut failed_since = None;

        assert!(!delivery_failure_grace_elapsed(
            true,
            &mut failed_since,
            started,
        ));
        assert!(delivery_failure_grace_elapsed(
            true,
            &mut failed_since,
            started + VIDEO_DELIVERY_FAILURE_GRACE,
        ));
    }

    #[tokio::test]
    async fn stalled_delivery_is_bounded() {
        let error = await_video_delivery(
            std::future::pending::<anyhow::Result<()>>(),
            Duration::from_millis(1),
        )
        .await
        .expect_err("a stalled sample write must time out");

        assert!(error.to_string().contains("delivery timed out"));
    }

    #[tokio::test]
    async fn encoded_frame_slot_replaces_stale_frames() {
        let slot = EncodedFrameSlot::new();
        assert!(slot.replace(EncodedFrame {
            bytes: vec![1],
            width: 2,
            height: 2,
        }));
        assert!(slot.replace(EncodedFrame {
            bytes: vec![2],
            width: 2,
            height: 2,
        }));

        let (frame, dropped) = slot.take().await.expect("newest frame");
        assert_eq!(frame.bytes, vec![2]);
        assert_eq!(dropped, 1);
        slot.close();
        assert!(slot.take().await.is_none());
    }

    #[tokio::test]
    async fn encoded_frame_slot_never_replaces_a_recovery_keyframe_with_delta() {
        let slot = EncodedFrameSlot::new();
        assert!(slot.replace(EncodedFrame {
            bytes: vec![0, 0, 0, 1, 0x65],
            width: 2,
            height: 2,
        }));
        assert!(slot.replace(EncodedFrame {
            bytes: vec![0, 0, 0, 1, 0x41],
            width: 2,
            height: 2,
        }));

        assert_eq!(
            slot.take().await.expect("recovery frame").0.bytes,
            vec![0, 0, 0, 1, 0x65]
        );
    }

    #[tokio::test]
    async fn encoded_frame_slot_close_wakes_a_waiting_consumer() {
        let slot = std::sync::Arc::new(EncodedFrameSlot::new());
        let consumer = {
            let slot = slot.clone();
            tokio::spawn(async move { slot.take().await })
        };
        slot.close();
        let result = tokio::time::timeout(Duration::from_secs(1), consumer)
            .await
            .expect("consumer must wake after close")
            .expect("consumer task must finish");
        assert!(result.is_none());
    }

    #[test]
    fn preserves_aspect_ratio_and_even_dimensions() {
        assert_eq!(fit_dimensions(3840, 2160, 1920, 1080), (1920, 1080));
        let (width, height) = fit_dimensions(1921, 1081, 1920, 1080);
        assert_eq!(width % 2, 0);
        assert_eq!(height % 2, 0);
    }

    #[test]
    fn capture_queue_replaces_obsolete_frames() {
        let queue = LatestFrame::default();
        assert!(queue.replace(xcap::Frame::new(1, 1, vec![1, 0, 0, 255])));
        assert!(queue.replace(xcap::Frame::new(1, 1, vec![2, 0, 0, 255])));
        let frame = queue.take(Duration::from_millis(1)).unwrap();
        assert_eq!(frame.frame.raw[0], 2);
    }

    #[test]
    fn bitrate_hysteresis_avoids_encoder_churn() {
        assert_eq!(bitrate_target(4_000, 3_500, 250, 8_000), None);
        assert_eq!(bitrate_target(4_000, 3_000, 250, 8_000), Some(3_000));
        assert_eq!(bitrate_target(3_000, 5_000, 250, 8_000), Some(5_000));
        assert_eq!(bitrate_target(4_000, 100, 250, 8_000), Some(250));
        assert_eq!(bitrate_target(4_000, 20_000, 250, 8_000), Some(8_000));
    }

    const TEST_SETTINGS: EncoderSettings = EncoderSettings {
        bitrate_kbps: 4_000,
        max_fps: 30,
        latency_mode: LatencyMode::Balanced,
    };

    struct FakeHardware {
        frames_left: usize,
        missing_headers: bool,
    }

    impl FrameEncoder for FakeHardware {
        fn force_keyframe(&mut self) {}

        fn set_bitrate(&mut self, _target_kbps: u32) -> anyhow::Result<()> {
            anyhow::bail!("simulated bitrate rejection")
        }

        fn encode(&mut self, image: &RgbaImage) -> anyhow::Result<EncodedFrame> {
            anyhow::ensure!(self.frames_left > 0, "simulated device loss");
            self.frames_left -= 1;
            Ok(EncodedFrame {
                bytes: if self.missing_headers {
                    vec![0, 0, 1, 0x41, 1]
                } else {
                    vec![0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
                },
                width: image.width(),
                height: image.height(),
            })
        }
    }

    const FAILING_HARDWARE: HardwareBackend = HardwareBackend {
        id: "first",
        name: "First native backend",
        open: |_, _| {
            Ok(Box::new(FakeHardware {
                frames_left: 1,
                missing_headers: false,
            }))
        },
    };
    const ALTERNATE: HardwareBackend = HardwareBackend {
        id: "second",
        name: "Second native backend",
        open: |_, _| {
            Ok(Box::new(FakeHardware {
                frames_left: 1,
                missing_headers: false,
            }))
        },
    };

    fn assert_decodable_idr(frame: &EncodedFrame) {
        let types = annex_b_nal_types(&frame.bytes).expect("Annex-B output");
        assert!([5, 7, 8].iter().all(|nal| types.contains(nal)));
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let decoded = decoder.decode(&frame.bytes).unwrap().expect("decoded IDR");
        use openh264::formats::YUVSource;
        assert_eq!(
            decoded.dimensions(),
            (frame.width as usize, frame.height as usize)
        );
    }

    #[test]
    fn unavailable_hardware_falls_back_only_in_auto() {
        const MISSING: &[HardwareBackend] = &[HardwareBackend {
            id: "missing",
            name: "Unavailable native backend",
            open: |_, _| anyhow::bail!("simulated missing driver"),
        }];
        let open = |preference| {
            EncoderBackend::with_hardware(preference, TEST_SETTINGS, (64, 64), MISSING)
        };
        assert_eq!(
            open(EncoderPreference::Auto).unwrap().id(),
            "software-openh264"
        );
        assert!(open(EncoderPreference::Hardware).is_err());
        const NEVER_OPEN: &[HardwareBackend] = &[HardwareBackend {
            id: "never",
            name: "Forbidden native backend",
            open: |_, _| panic!("software mode probed a hardware driver"),
        }];
        let software = EncoderBackend::with_hardware(
            EncoderPreference::Software,
            TEST_SETTINGS,
            (64, 64),
            NEVER_OPEN,
        )
        .unwrap();
        assert_eq!(software.id(), "software-openh264");
    }

    #[test]
    fn active_hardware_failure_reencodes_same_frame_without_reprobing() {
        let image = RgbaImage::from_pixel(64, 64, Rgba([32, 96, 160, 255]));
        let mut encoder = EncoderBackend::with_hardware(
            EncoderPreference::Auto,
            TEST_SETTINGS,
            image.dimensions(),
            &[FAILING_HARDWARE],
        )
        .unwrap();
        encoder.encode(&image).unwrap();
        assert_eq!(encoder.id(), "first");
        assert_decodable_idr(&encoder.encode(&image).unwrap());
        assert_eq!(encoder.id(), "software-openh264");
        encoder.encode(&image).unwrap();
        assert_eq!(encoder.id(), "software-openh264");
    }

    #[test]
    fn hardware_mode_tries_another_native_backend_but_never_software() {
        let image = RgbaImage::from_pixel(64, 64, Rgba([32, 96, 160, 255]));
        let mut encoder = EncoderBackend::with_hardware(
            EncoderPreference::Hardware,
            TEST_SETTINGS,
            image.dimensions(),
            &[FAILING_HARDWARE, ALTERNATE],
        )
        .unwrap();
        encoder.encode(&image).unwrap();
        encoder.encode(&image).unwrap();
        assert_eq!(encoder.id(), "second");
        assert!(encoder.encode(&image).is_err());
    }

    #[test]
    fn auto_recovers_from_missing_initial_headers_or_native_bitrate_failure() {
        const NO_HEADERS: &[HardwareBackend] = &[HardwareBackend {
            id: "no-headers",
            name: "Native backend without headers",
            open: |_, _| {
                Ok(Box::new(FakeHardware {
                    frames_left: 1,
                    missing_headers: true,
                }))
            },
        }];
        let image = RgbaImage::from_pixel(64, 64, Rgba([32, 96, 160, 255]));
        let open = |backends| {
            EncoderBackend::with_hardware(
                EncoderPreference::Auto,
                TEST_SETTINGS,
                image.dimensions(),
                backends,
            )
            .unwrap()
        };
        let mut no_headers = open(NO_HEADERS);
        assert_decodable_idr(&no_headers.encode(&image).unwrap());
        assert_eq!(no_headers.id(), "software-openh264");
        let mut retune_failed = open(&[FAILING_HARDWARE]);
        retune_failed.encode(&image).unwrap();
        assert!(retune_failed.apply_bitrate_estimate(2_000, 4_000).unwrap());
        assert_eq!(retune_failed.settings.bitrate_kbps, 2_000);
        assert_eq!(retune_failed.id(), "software-openh264");
        assert_decodable_idr(&retune_failed.encode(&image).unwrap());
    }

    #[test]
    fn auto_encodes_a_decodable_idr_at_start_and_after_resize() {
        let mut encoder = EncoderBackend::new(
            EncoderPreference::Auto,
            4_000,
            30,
            LatencyMode::Balanced,
            (1_920, 1_080),
        )
        .expect("auto encoder");
        for (width, height) in [(1_920, 1_080), (1_280, 720)] {
            let image = RgbaImage::from_pixel(width, height, Rgba([32, 96, 160, 255]));
            let frame = encoder
                .encode(&image)
                .expect("auto encoder must encode or fall back");
            assert_eq!((frame.width, frame.height), (width, height));
            assert_decodable_idr(&frame);
            eprintln!("{} encoded {width}x{height}", encoder.name());
        }
    }

    #[test]
    #[ignore = "opt-in encode-only performance measurement"]
    fn benchmark_encoder() {
        let requested =
            std::env::var("NANOCTL_BENCH_ENCODER").unwrap_or("software-openh264".into());
        let dimensions = (1_920, 1_080);
        let mut encoder = if requested == "software-openh264" {
            EncoderBackend::with_hardware(
                EncoderPreference::Software,
                TEST_SETTINGS,
                dimensions,
                &[],
            )
        } else {
            let index = HARDWARE_BACKENDS
                .iter()
                .position(|candidate| candidate.id == requested)
                .expect("unknown encoder ID on this platform");
            EncoderBackend::with_hardware(
                EncoderPreference::Hardware,
                TEST_SETTINGS,
                dimensions,
                &HARDWARE_BACKENDS[index..=index],
            )
        }
        .expect("requested encoder unavailable");
        let mut image = RgbaImage::from_fn(dimensions.0, dimensions.1, |x, y| {
            if (x / 48 + y / 48) % 2 == 0 {
                Rgba([46, 51, 56, 255])
            } else {
                Rgba([24, 30, 36, 255])
            }
        });
        assert_decodable_idr(&encoder.encode(&image).unwrap());
        const WARMUP: u32 = 9;
        const FRAMES: u32 = 180;
        let mut encode_micros = Vec::new();
        let mut output_frames = 0;
        for index in 0..WARMUP + FRAMES {
            let x = (index * 8) % (dimensions.0 - 256);
            for y in 420..548 {
                for column in x..x + 16 {
                    image.put_pixel(column, y, Rgba([(index % 255) as u8, 96, 160, 255]));
                }
            }
            let started = Instant::now();
            let frame = encoder.encode(&image).expect("encode benchmark frame");
            let elapsed = started.elapsed();
            if index >= WARMUP {
                encode_micros.push(elapsed.as_micros() as u64);
                output_frames += u32::from(!frame.bytes.is_empty());
            }
        }
        encode_micros.sort_unstable();
        assert!(output_frames > 0);
        println!(
            "backend={} input={}x{} measured={} output={} p50_ms={:.2} p95_ms={:.2}",
            encoder.id(),
            dimensions.0,
            dimensions.1,
            FRAMES,
            output_frames,
            super::percentile_milliseconds(&encode_micros, 0.50),
            super::percentile_milliseconds(&encode_micros, 0.95),
        );
    }

    #[test]
    fn converts_avcc_access_units_and_parameter_sets_to_annex_b() {
        let avcc = [0, 0, 0, 3, 0x65, 1, 2, 0, 0, 0, 2, 0x41, 3];
        let annex_b = avcc_to_annex_b(&avcc, 4, &[&[0x67, 4], &[0x68, 5]]).unwrap();
        assert_eq!(
            annex_b,
            [
                0, 0, 0, 1, 0x67, 4, 0, 0, 0, 1, 0x68, 5, 0, 0, 0, 1, 0x65, 1, 2, 0, 0, 0, 1, 0x41,
                3,
            ]
        );
    }

    #[test]
    fn rejects_truncated_or_empty_avcc_nals() {
        assert!(avcc_to_annex_b(&[0, 0, 0], 4, &[]).is_err());
        assert!(avcc_to_annex_b(&[0, 0, 0, 0], 4, &[]).is_err());
        assert!(avcc_to_annex_b(&[0, 0, 0, 4, 0x65], 4, &[]).is_err());
        assert!(avcc_to_annex_b(&[1, 0x65], 0, &[]).is_err());
    }

    #[test]
    fn indexes_annex_b_parameter_sets_and_frames() {
        let types =
            annex_b_nal_types(&[0, 0, 0, 1, 0x67, 4, 0, 0, 1, 0x68, 5, 0, 0, 0, 1, 0x65, 6])
                .unwrap();
        assert_eq!(types, [7, 8, 5]);
        assert!(annex_b_nal_types(&[0, 0, 0, 2, 0x65]).is_err());
        assert!(annex_b_nal_types(&[0, 0, 0, 1]).is_err());
    }
}
