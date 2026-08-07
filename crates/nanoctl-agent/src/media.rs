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
    slot: Mutex<Option<xcap::Frame>>,
    ready: Condvar,
}

impl LatestFrame {
    fn replace(&self, frame: xcap::Frame) -> bool {
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        *slot = Some(frame);
        self.ready.notify_one();
        true
    }

    fn take(&self, timeout: Duration) -> Result<xcap::Frame> {
        let slot = self
            .slot
            .lock()
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        let (mut slot, timeout) = self
            .ready
            .wait_timeout_while(slot, timeout, |frame| frame.is_none())
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        if timeout.timed_out() && slot.is_none() {
            bail!("display capture timed out");
        }
        slot.take()
            .context("display capture stopped without a frame")
    }

    fn take_until_stopped(
        &self,
        timeout: Duration,
        stopped: &AtomicBool,
    ) -> Result<Option<xcap::Frame>> {
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("display capture timed out");
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
    recorder: xcap::VideoRecorder,
    latest_frame: Arc<LatestFrame>,
    encoder: EncoderBackend,
}

enum EncoderBackend {
    #[cfg(target_os = "linux")]
    VaApi(Box<crate::linux_encoder::LinuxEncoder>),
    #[cfg(target_os = "macos")]
    VideoToolbox(Box<crate::macos_encoder::MacOsEncoder>),
    #[cfg(target_os = "windows")]
    MediaFoundation {
        encoder: Box<crate::windows_encoder::WindowsEncoder>,
        allow_software_fallback: bool,
    },
    Software(Box<SoftwareEncoder>),
}

struct SoftwareEncoder {
    encoder: Encoder,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
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
        let (recorder, latest_frame) = start_capture(&monitor)?;
        Ok(Self {
            recorder,
            latest_frame,
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
        let (recorder, latest_frame) = start_capture(&monitor)?;
        let _ = self.recorder.stop();
        self.recorder = recorder;
        self.latest_frame = latest_frame;
        self.encoder.force_keyframe();
        Ok(())
    }

    pub fn apply_bitrate_estimate(
        &mut self,
        estimate_kbps: u32,
        ceiling_kbps: u32,
    ) -> Result<bool> {
        self.encoder
            .apply_bitrate_estimate(estimate_kbps, ceiling_kbps)
    }

    pub fn next_access_unit(&mut self, max_width: u32, max_height: u32) -> Result<EncodedFrame> {
        let frame = self.latest_frame.take(Duration::from_secs(5))?;
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
        self.encoder.encode(&image)
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
        if stopped.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(self.encoder.encode(&image)?))
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
        #[cfg(target_os = "linux")]
        if !matches!(preference, EncoderPreference::Software) {
            match crate::linux_encoder::LinuxEncoder::new(bitrate_kbps, max_fps, latency_mode) {
                Ok(encoder) => return Ok(Self::VaApi(Box::new(encoder))),
                Err(error) if matches!(preference, EncoderPreference::Hardware) => {
                    return Err(error).context("required VA-API encoder is unavailable");
                }
                Err(error) => {
                    tracing::warn!(%error, "VA-API unavailable; using OpenH264");
                }
            }
        }
        #[cfg(target_os = "macos")]
        if !matches!(preference, EncoderPreference::Software) {
            match crate::macos_encoder::MacOsEncoder::new(bitrate_kbps, max_fps, latency_mode) {
                Ok(encoder) => return Ok(Self::VideoToolbox(Box::new(encoder))),
                Err(error) if matches!(preference, EncoderPreference::Hardware) => {
                    return Err(error).context("required VideoToolbox encoder is unavailable");
                }
                Err(error) => {
                    tracing::warn!(%error, "VideoToolbox unavailable; using OpenH264");
                }
            }
        }
        #[cfg(target_os = "windows")]
        if !matches!(preference, EncoderPreference::Software) {
            match crate::windows_encoder::WindowsEncoder::new(
                bitrate_kbps,
                max_fps,
                latency_mode,
                probe_dimensions.0,
                probe_dimensions.1,
            ) {
                Ok(encoder) => {
                    return Ok(Self::MediaFoundation {
                        encoder: Box::new(encoder),
                        allow_software_fallback: matches!(preference, EncoderPreference::Auto),
                    });
                }
                Err(error) if matches!(preference, EncoderPreference::Hardware) => {
                    return Err(error)
                        .context("required Media Foundation hardware encoder is unavailable");
                }
                Err(error) => {
                    tracing::warn!(%error, "Media Foundation hardware unavailable; using OpenH264");
                }
            }
        }
        if matches!(preference, EncoderPreference::Hardware) {
            bail!("hardware encoding was required but no verified native backend is available");
        }
        #[cfg(not(target_os = "windows"))]
        let _ = probe_dimensions;
        Ok(Self::Software(Box::new(SoftwareEncoder::new(
            bitrate_kbps,
            max_fps,
            latency_mode,
        )?)))
    }

    fn force_keyframe(&mut self) {
        match self {
            #[cfg(target_os = "linux")]
            Self::VaApi(encoder) => encoder.force_keyframe(),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(encoder) => encoder.force_keyframe(),
            #[cfg(target_os = "windows")]
            Self::MediaFoundation { encoder, .. } => encoder.force_keyframe(),
            Self::Software(encoder) => encoder.encoder.force_intra_frame(),
        }
    }

    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        match self {
            #[cfg(target_os = "linux")]
            Self::VaApi(encoder) => encoder.apply_bitrate_estimate(estimate_kbps, ceiling_kbps),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(encoder) => {
                encoder.apply_bitrate_estimate(estimate_kbps, ceiling_kbps)
            }
            #[cfg(target_os = "windows")]
            Self::MediaFoundation { encoder, .. } => {
                encoder.apply_bitrate_estimate(estimate_kbps, ceiling_kbps)
            }
            Self::Software(encoder) => encoder.apply_bitrate_estimate(estimate_kbps, ceiling_kbps),
        }
    }

    fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        match self {
            #[cfg(target_os = "linux")]
            Self::VaApi(encoder) => encoder.encode(image),
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(encoder) => encoder.encode(image),
            #[cfg(target_os = "windows")]
            Self::MediaFoundation {
                encoder,
                allow_software_fallback,
            } => match encoder.encode(image) {
                Ok(frame) => Ok(frame),
                Err(error) if *allow_software_fallback => {
                    let bitrate_kbps = encoder.bitrate_kbps();
                    let max_fps = encoder.max_fps();
                    let latency_mode = encoder.latency_mode();
                    tracing::warn!(
                        error = %error,
                        "Media Foundation failed on a captured frame; switching session to OpenH264"
                    );
                    let mut software = SoftwareEncoder::new(bitrate_kbps, max_fps, latency_mode)
                        .context("Media Foundation failed and OpenH264 fallback is unavailable")?;
                    let frame = software
                        .encode(image)
                        .context("OpenH264 fallback could not encode the captured frame")?;
                    *self = Self::Software(Box::new(software));
                    Ok(frame)
                }
                Err(error) => Err(error),
            },
            Self::Software(encoder) => encoder.encode(image),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::VaApi(_) => "VA-API",
            #[cfg(target_os = "macos")]
            Self::VideoToolbox(_) => "VideoToolbox",
            #[cfg(target_os = "windows")]
            Self::MediaFoundation { .. } => "Media Foundation hardware MFT",
            Self::Software(_) => "OpenH264",
        }
    }
}

impl SoftwareEncoder {
    fn new(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Self> {
        Ok(Self {
            encoder: create_encoder(bitrate_kbps, max_fps, latency_mode)?,
            bitrate_kbps,
            max_fps,
            latency_mode,
            rgb: Vec::new(),
            yuv: None,
        })
    }

    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        let floor_kbps = 250.min(ceiling_kbps);
        let Some(target_kbps) =
            bitrate_target(self.bitrate_kbps, estimate_kbps, floor_kbps, ceiling_kbps)
        else {
            return Ok(false);
        };
        self.encoder = create_encoder(target_kbps, self.max_fps, self.latency_mode)?;
        self.bitrate_kbps = target_kbps;
        Ok(true)
    }

    fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        let dimensions = (image.width() as usize, image.height() as usize);
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

fn start_capture(monitor: &Monitor) -> Result<(xcap::VideoRecorder, Arc<LatestFrame>)> {
    let (recorder, native_frames) = monitor
        .video_recorder()
        .context("continuous display capture is unavailable")?;
    let latest_frame = Arc::new(LatestFrame::default());
    let forwarding_slot = Arc::downgrade(&latest_frame);
    std::thread::Builder::new()
        .name("nanoctl-capture".to_owned())
        .spawn(move || {
            while let Ok(frame) = native_frames.recv() {
                let Some(latest_frame) = forwarding_slot.upgrade() else {
                    break;
                };
                // Replacement, rather than append, makes this queue exactly one frame deep.
                if !latest_frame.replace(frame) {
                    break;
                }
            }
        })
        .context("capture forwarding thread could not start")?;
    recorder
        .start()
        .context("continuous display capture could not start")?;
    Ok((recorder, latest_frame))
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

pub fn probe_encoder(preference: EncoderPreference) -> Result<&'static str> {
    let encoder = EncoderBackend::new(preference, 1_000, 30, LatencyMode::Balanced, (1_280, 720))?;
    Ok(encoder.name())
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
        let _ = self.recorder.stop();
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
             dimensions={}x{} idr={} sps={} pps={}",
            if self.passed { "pass" } else { "fail" },
            self.agent_version,
            self.target_os,
            self.target_arch,
            self.backend,
            self.frames,
            self.frames_per_second,
            self.average_bitrate_kbps,
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
    while Instant::now() < deadline {
        let now = Instant::now();
        if now < next_frame {
            std::thread::sleep(next_frame - now);
        }
        next_frame = Instant::now() + frame_interval;
        let frame = encoder.next_access_unit(quality.max_width, quality.max_height)?;
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
        width: dimensions.0,
        height: dimensions.1,
        idr_frames,
        sps_units,
        pps_units,
    })
}

fn annex_b_nal_types(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > 32 * 1024 * 1024 {
        bail!("H.264 access unit exceeds the smoke-test safety bound");
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
        let mut write_result = Ok(());
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                frame = encoded_slot.take() => {
                    let Some((frame, dropped)) = frame else { break };
                    // Tell the RTP packetizer about frames discarded by the newest-frame slot so
                    // timestamps keep pace with the capture clock instead of falling behind.
                    let sample = Sample {
                        data: frame.bytes.into(),
                        duration,
                        prev_dropped_packets: dropped,
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
                        }
                        Ok(()) => {}
                        Err(error) => {
                            delivery_error_count = delivery_error_count.saturating_add(1);
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
        EncodedFrame, EncodedFrameSlot, EncoderBackend, LatestFrame, SoftwareEncoder,
        VIDEO_DELIVERY_FAILURE_GRACE, annex_b_nal_types, avcc_to_annex_b, await_video_delivery,
        bitrate_target, delivery_failure_grace_elapsed, fit_dimensions,
    };
    use crate::config::{EncoderPreference, LatencyMode};
    use std::time::{Duration, Instant};

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
        assert_eq!(frame.raw[0], 2);
    }

    #[test]
    fn bitrate_hysteresis_avoids_encoder_churn() {
        assert_eq!(bitrate_target(4_000, 3_500, 250, 8_000), None);
        assert_eq!(bitrate_target(4_000, 3_000, 250, 8_000), Some(3_000));
        assert_eq!(bitrate_target(3_000, 5_000, 250, 8_000), Some(5_000));
        assert_eq!(bitrate_target(4_000, 100, 250, 8_000), Some(250));
        assert_eq!(bitrate_target(4_000, 20_000, 250, 8_000), Some(8_000));
    }

    #[test]
    fn required_hardware_never_silently_falls_back() {
        if let Ok(backend) = EncoderBackend::new(
            EncoderPreference::Hardware,
            4_000,
            60,
            LatencyMode::Balanced,
            (1_920, 1_080),
        ) {
            assert_ne!(backend.name(), "OpenH264");
        }
    }

    #[test]
    fn software_fallback_encodes_a_real_1080p_capture_frame() {
        let image =
            xcap::image::RgbaImage::from_pixel(1920, 1080, xcap::image::Rgba([32, 96, 160, 255]));
        let mut encoder =
            SoftwareEncoder::new(4_000, 30, LatencyMode::Balanced).expect("OpenH264 fallback");
        let frame = encoder.encode(&image).expect("encode 1080p capture frame");
        assert_eq!((frame.width, frame.height), (1920, 1080));
        assert!(!frame.bytes.is_empty());
        let types = annex_b_nal_types(&frame.bytes).expect("Annex-B output");
        assert!(types.contains(&7));
        assert!(types.contains(&8));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_auto_encoder_survives_the_first_real_1080p_frame() {
        let image =
            xcap::image::RgbaImage::from_pixel(1920, 1080, xcap::image::Rgba([32, 96, 160, 255]));
        let mut encoder = EncoderBackend::new(
            EncoderPreference::Auto,
            4_000,
            30,
            LatencyMode::Balanced,
            (1_920, 1_080),
        )
        .expect("Windows auto encoder");
        let frame = encoder
            .encode(&image)
            .expect("Windows auto encoder must encode or fall back");
        assert_eq!((frame.width, frame.height), (1920, 1080));
        assert!(!frame.bytes.is_empty());
        assert!(annex_b_nal_types(&frame.bytes).is_ok());
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
