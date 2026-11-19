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
use openh264::formats::YUVSource;
use openh264_sys2::{
    ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_0,
    SPATIAL_LAYER_ALL,
};
use xcap::Monitor;

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

#[cfg(target_os = "linux")]
#[path = "linux_damage.rs"]
mod linux_damage;

use crate::adaptive::{DamagePacer, PaceAction, RateController, frame_changed};
use crate::config::QualityConfig;
use crate::config::{EncoderPreference, LatencyMode};

const VIDEO_DELIVERY_FAILURE_GRACE: Duration = Duration::from_secs(15);
const VIDEO_SAMPLE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct SendRateWindow {
    writes: VecDeque<(Instant, usize)>,
}

impl SendRateWindow {
    fn kbps(&mut self, now: Instant) -> f64 {
        while self
            .writes
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > Duration::from_secs(2))
        {
            self.writes.pop_front();
        }
        self.writes.iter().map(|(_, bytes)| *bytes).sum::<usize>() as f64 * 8.0 / 2000.0
    }
}

#[derive(Default)]
pub struct PipelineStats {
    send_window: Mutex<SendRateWindow>,
    pub send_kbps: AtomicU32,
    #[cfg(target_os = "linux")]
    pub capture_parked: AtomicBool,
    #[cfg(target_os = "linux")]
    pub damage_enabled: AtomicBool,
    #[cfg(target_os = "linux")]
    pub damage_resumes: AtomicU64,
    #[cfg(target_os = "linux")]
    pub first_change_samples: AtomicU64,
    #[cfg(target_os = "linux")]
    pub first_change_last_ns: AtomicU64,
    #[cfg(target_os = "linux")]
    pub first_change_max_ns: AtomicU64,
    #[cfg(target_os = "linux")]
    pub first_change_capture_ready_ns: AtomicU64,
    #[cfg(target_os = "linux")]
    pub first_change_encode_ns: AtomicU64,
    #[cfg(target_os = "linux")]
    damage_started: Mutex<Option<Instant>>,
    pub frames_captured: AtomicU64,
    pub frames_encoded: AtomicU64,
    pub changed_encodes: AtomicU64,
    pub unchanged_encodes: AtomicU64,
    pub skipped_unchanged: AtomicU64,
    pub keepalives: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub target_kbps: AtomicU32,
    pub target_fps: AtomicU16,
    pub encoded_width: AtomicU32,
    pub encoded_height: AtomicU32,
    pub fresh_frames: AtomicU64,
    pub compare_ns: AtomicU64,
    pub capture_wait_ns: AtomicU64,
    pub preprocess_ns: AtomicU64,
    pub backend_ns: AtomicU64,
    pub color_convert_ns: AtomicU64,
    pub encode_attempts: AtomicU64,
    pub empty_encodes: AtomicU64,
    pub pacer_skips: AtomicU64,
    #[cfg(target_os = "linux")]
    pub producer_tid: AtomicU32,
}

impl PipelineStats {
    fn log(&self) {
        tracing::info!(
            frames_captured = self.frames_captured.load(Ordering::Relaxed),
            frames_encoded = self.frames_encoded.load(Ordering::Relaxed),
            skipped_unchanged = self.skipped_unchanged.load(Ordering::Relaxed),
            keepalives = self.keepalives.load(Ordering::Relaxed),
            bytes_sent = self.bytes_sent.load(Ordering::Relaxed),
            send_kbps = self.send_kbps.load(Ordering::Relaxed),
            target_kbps = self.target_kbps.load(Ordering::Relaxed),
            target_fps = self.target_fps.load(Ordering::Relaxed),
            encoded_width = self.encoded_width.load(Ordering::Relaxed),
            encoded_height = self.encoded_height.load(Ordering::Relaxed),
            "video pipeline statistics"
        );
    }
}

struct EncodeScale(u32);

impl EncodeScale {
    fn new(kbps: u32) -> Self {
        Self(if kbps < 600 {
            4
        } else if kbps < 1500 {
            2
        } else {
            1
        })
    }

    fn update(&mut self, kbps: u32) -> bool {
        let previous = self.0;
        self.0 = match self.0 {
            1 if kbps < 1500 => Self::new(kbps).0,
            2 if kbps < 600 => 4,
            4 if kbps >= 800 => {
                if kbps >= 1800 {
                    1
                } else {
                    2
                }
            }
            2 if kbps >= 1800 => 1,
            _ => self.0,
        };
        self.0 != previous
    }

    fn limits(&self, width: u32, height: u32) -> (u32, u32) {
        ((width / self.0).max(2), (height / self.0).max(2))
    }
}

fn sample_duration(previous: Option<Instant>, now: Instant, fps: u16) -> Duration {
    previous
        .map_or_else(
            || Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
            |previous| now.saturating_duration_since(previous),
        )
        .clamp(Duration::from_millis(1), Duration::from_secs(5))
}

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
    slot: Mutex<Option<Arc<CapturedFrame>>>,
    last: Mutex<Option<Arc<CapturedFrame>>>,
    ready: Condvar,
    stopped: AtomicBool,
    finished: AtomicBool,
    stats: Arc<PipelineStats>,
}

#[derive(Clone)]
struct CapturedFrame {
    frame: xcap::Frame,
    captured_at: Instant,
}

impl CapturedFrame {
    fn differs_from(&self, previous: &Self) -> bool {
        self.frame.width != previous.frame.width
            || self.frame.height != previous.frame.height
            || frame_changed(&previous.frame.raw, &self.frame.raw)
    }
}

impl LatestFrame {
    fn replace(&self, frame: xcap::Frame) -> bool {
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        let captured = Arc::new(CapturedFrame {
            frame,
            captured_at: Instant::now(),
        });
        if let Ok(mut last) = self.last.lock() {
            *last = Some(captured.clone());
        }
        *slot = Some(captured);
        self.stats.frames_captured.fetch_add(1, Ordering::Relaxed);
        self.ready.notify_one();
        true
    }

    fn take(&self, timeout: Duration) -> Result<Arc<CapturedFrame>> {
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
    ) -> Result<Option<Arc<CapturedFrame>>> {
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
            if self.finished.load(Ordering::Acquire) {
                bail!("display capture forwarding thread stopped");
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
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
    dimensions: Option<(u32, u32)>,
    yuv: Option<I420Frame>,
    color_convert_ns: u64,
}

struct I420Frame {
    width: usize,
    height: usize,
    bytes: Vec<u8>,
}

impl I420Frame {
    fn new(width: usize, height: usize) -> Result<Self> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            bail!("I420 dimensions must be non-zero and even");
        }
        let pixels = width
            .checked_mul(height)
            .context("I420 dimensions overflow")?;
        let length = pixels
            .checked_add(pixels / 2)
            .context("I420 size overflows")?;
        Ok(Self {
            width,
            height,
            bytes: vec![0; length],
        })
    }

    fn read_rgba(&mut self, rgba: &[u8]) -> Result<()> {
        let pixels = self.width * self.height;
        if Some(rgba.len()) != pixels.checked_mul(4) {
            bail!("RGBA pixel buffer does not match I420 dimensions");
        }
        let (y, uv) = self.bytes.split_at_mut(pixels);
        let (u, v) = uv.split_at_mut(pixels / 4);
        for (pixel, y) in rgba.chunks_exact(4).zip(y) {
            *y = (((66 * u32::from(pixel[0])
                + 129 * u32::from(pixel[1])
                + 25 * u32::from(pixel[2]))
                >> 8)
                + 16) as u8;
        }
        for ((rows, u), v) in rgba
            .chunks_exact(self.width * 8)
            .zip(u.chunks_exact_mut(self.width / 2))
            .zip(v.chunks_exact_mut(self.width / 2))
        {
            let (top, bottom) = rows.split_at(self.width * 4);
            for (((top, bottom), u), v) in top
                .chunks_exact(8)
                .zip(bottom.chunks_exact(8))
                .zip(u)
                .zip(v)
            {
                let average = |channel| {
                    (i16::from(top[channel])
                        + i16::from(top[channel + 4])
                        + i16::from(bottom[channel])
                        + i16::from(bottom[channel + 4])
                        + 2)
                        / 4
                };
                let (r, g, b) = (average(0), average(1), average(2));
                *u = (((-38 * r + 112 * b - 74 * g) >> 8) + 128) as u8;
                *v = (((112 * r - 18 * b - 94 * g) >> 8) + 128) as u8;
            }
        }
        Ok(())
    }
}

impl YUVSource for I420Frame {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }
    fn y(&self) -> &[u8] {
        &self.bytes[..self.width * self.height]
    }
    fn u(&self) -> &[u8] {
        &self.bytes[self.width * self.height..self.width * self.height * 5 / 4]
    }
    fn v(&self) -> &[u8] {
        &self.bytes[self.width * self.height * 5 / 4..]
    }
}

impl CaptureEncoder {
    pub fn primary(
        max_bitrate_kbps: u32,
        max_fps: u16,
        max_width: u32,
        max_height: u32,
        latency_mode: LatencyMode,
        encoder_preference: EncoderPreference,
        stats: Arc<PipelineStats>,
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
        let (recorder, latest_frame, forwarder) = start_capture(&monitor, stats)?;
        Ok(Self {
            recorder,
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
        let (recorder, latest_frame, forwarder) =
            start_capture(&monitor, self.latest_frame.stats.clone())?;
        self.latest_frame.stopped.store(true, Ordering::Release);
        if let Some(old_recorder) = self.recorder.take() {
            let _ = old_recorder.stop();
            drop(old_recorder);
        }
        if let Some(old_forwarder) = self.forwarder.take() {
            let _ = old_forwarder.join();
        }
        self.recorder = recorder;
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
        self.encode_captured_frame(&captured, max_width, max_height)
    }

    fn encode_captured_frame(
        &mut self,
        captured: &CapturedFrame,
        max_width: u32,
        max_height: u32,
    ) -> Result<(EncodedFrame, TimingSample)> {
        let CapturedFrame { frame, captured_at } = captured;
        let preprocess_started = Instant::now();
        let source = rgba_view(frame)?;
        let (source_width, source_height) = source.dimensions();
        let (target_width, target_height) =
            fit_dimensions(source_width, source_height, max_width, max_height);
        let resized;
        let image = if target_width == source_width && target_height == source_height {
            source
        } else {
            resized = xcap::image::imageops::resize(
                &source,
                target_width,
                target_height,
                xcap::image::imageops::FilterType::Triangle,
            );
            xcap::image::ImageBuffer::from_raw(
                target_width,
                target_height,
                resized.as_raw().as_slice(),
            )
            .context("resized capture returned an invalid RGBA frame")?
        };
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            bail!("captured display dimensions must be non-zero and even");
        }
        let encode_started = Instant::now();
        self.latest_frame.stats.preprocess_ns.fetch_add(
            encode_started.duration_since(preprocess_started).as_nanos() as u64,
            Ordering::Relaxed,
        );
        self.latest_frame
            .stats
            .encode_attempts
            .fetch_add(1, Ordering::Relaxed);
        let color_before = self.encoder.color_convert_ns();
        let encoded = self.encoder.encode(&image)?;
        let encoded_at = Instant::now();
        self.latest_frame.stats.backend_ns.fetch_add(
            encoded_at.duration_since(encode_started).as_nanos() as u64,
            Ordering::Relaxed,
        );
        self.latest_frame.stats.color_convert_ns.fetch_add(
            self.encoder.color_convert_ns().saturating_sub(color_before),
            Ordering::Relaxed,
        );
        if encoded.bytes.is_empty() {
            self.latest_frame
                .stats
                .empty_encodes
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok((
            encoded,
            TimingSample {
                capture_to_encode: encoded_at.saturating_duration_since(*captured_at),
                encode: encoded_at.saturating_duration_since(encode_started),
            },
        ))
    }
}

impl EncoderBackend {
    fn color_convert_ns(&self) -> u64 {
        match self {
            Self::Software(encoder) => encoder.color_convert_ns,
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            _ => 0,
        }
    }

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

    fn encode(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
    ) -> Result<EncodedFrame> {
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
            dimensions: None,
            yuv: None,
            color_convert_ns: 0,
        })
    }

    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        let floor_kbps = 250.min(ceiling_kbps);
        let Some(target_kbps) =
            bitrate_target(self.bitrate_kbps, estimate_kbps, floor_kbps, ceiling_kbps)
        else {
            return Ok(false);
        };
        if self.dimensions.is_some() {
            let mut bitrate = SBitrateInfo {
                iLayer: SPATIAL_LAYER_0,
                iBitrate: ceiling_kbps.saturating_mul(1_000).min(i32::MAX as u32) as i32,
            };
            // SAFETY: The initialized encoder consumes this matching option structure
            // synchronously. Keep the layer maximum above the new target before raising it.
            let result = unsafe {
                self.encoder.raw_api().set_option(
                    ENCODER_OPTION_MAX_BITRATE,
                    std::ptr::from_mut(&mut bitrate).cast(),
                )
            };
            if result != 0 {
                bail!("OpenH264 maximum bitrate update failed: {result}");
            }
            let mut bitrate = SBitrateInfo {
                iLayer: SPATIAL_LAYER_ALL,
                iBitrate: target_kbps.saturating_mul(1_000).min(i32::MAX as u32) as i32,
            };
            // SAFETY: The encoder is initialized, and SetOption consumes this correctly typed
            // bitrate structure synchronously without changing dimensions or buffer ownership.
            let result = unsafe {
                self.encoder.raw_api().set_option(
                    ENCODER_OPTION_BITRATE,
                    std::ptr::from_mut(&mut bitrate).cast(),
                )
            };
            if result != 0 {
                bail!("OpenH264 bitrate update failed: {result}");
            }
        }
        self.bitrate_kbps = target_kbps;
        Ok(true)
    }

    fn encode(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
    ) -> Result<EncodedFrame> {
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
        let color_started = Instant::now();
        if self
            .yuv
            .as_ref()
            .is_none_or(|frame| frame.dimensions() != dimensions)
        {
            self.yuv = Some(I420Frame::new(dimensions.0, dimensions.1)?);
        }
        let yuv = self.yuv.as_mut().context("I420 buffer is unavailable")?;
        yuv.read_rgba(image.as_raw())?;
        self.color_convert_ns += color_started.elapsed().as_nanos() as u64;
        let stream = self.encoder.encode(yuv).context("H.264 encode failed")?;
        Ok(EncodedFrame {
            bytes: stream.to_vec(),
            width: image.width(),
            height: image.height(),
        })
    }
}

fn rgba_view(
    frame: &xcap::Frame,
) -> Result<xcap::image::ImageBuffer<xcap::image::Rgba<u8>, &[u8]>> {
    xcap::image::ImageBuffer::from_raw(frame.width, frame.height, frame.raw.as_slice())
        .context("display capture returned an invalid RGBA frame")
}

fn start_capture(
    monitor: &Monitor,
    stats: Arc<PipelineStats>,
) -> Result<(
    Option<xcap::VideoRecorder>,
    Arc<LatestFrame>,
    std::thread::JoinHandle<()>,
)> {
    #[cfg(target_os = "linux")]
    match linux_damage::start(monitor, stats.clone()) {
        Ok((latest, thread)) => return Ok((None, latest, thread)),
        Err(error) => tracing::debug!(%error, "XDamage unavailable; retaining continuous capture"),
    }
    #[cfg(target_os = "linux")]
    {
        stats.damage_enabled.store(false, Ordering::Relaxed);
        stats.capture_parked.store(false, Ordering::Relaxed);
    }
    let (recorder, native_frames) = monitor
        .video_recorder()
        .context("continuous display capture is unavailable")?;
    let latest_frame = Arc::new(LatestFrame {
        stats,
        ..Default::default()
    });
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
            if let Some(latest_frame) = forwarding_slot.upgrade() {
                latest_frame.finished.store(true, Ordering::Release);
                latest_frame.ready.notify_one();
            }
        })
        .context("capture forwarding thread could not start")?;
    if let Err(error) = recorder.start() {
        latest_frame.stopped.store(true, Ordering::Release);
        let _ = recorder.stop();
        let _ = forwarder.join();
        return Err(error).context("continuous display capture could not start");
    }
    Ok((Some(recorder), latest_frame, forwarder))
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
    (target != current).then_some(target)
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
        Arc::default(),
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
    pub stats: Arc<PipelineStats>,
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
        self.stats.log();
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
            stats: Arc::default(),
        }
    }
}

pub fn spawn_video(
    track: Arc<TrackLocalStaticSample>,
    keyframe_requests: tokio::sync::watch::Receiver<u64>,
    link_feedback: tokio::sync::watch::Receiver<Option<crate::adaptive::LinkSample>>,
    display_selection: tokio::sync::watch::Receiver<String>,
    display_selection_feedback: tokio::sync::watch::Sender<String>,
    quality: VideoQuality,
) -> VideoPipeline {
    let stopped = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(PipelineStats::default());
    let controller = RateController::new(quality.max_bitrate_kbps, quality.max_fps);
    let initial = controller.decision();
    stats
        .target_kbps
        .store(initial.bitrate_kbps, Ordering::Relaxed);
    stats.target_fps.store(initial.fps, Ordering::Relaxed);
    let writer_stats = stats.clone();
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let worker_stopped = stopped.clone();
    let task = tokio::spawn(async move {
        let encoded_slot = Arc::new(EncodedFrameSlot::new());
        let (delivery_recovery, mut delivery_recovery_requests) =
            tokio::sync::watch::channel(0_u64);
        let producer_slot = encoded_slot.clone();
        let producer_stopped = worker_stopped.clone();
        let producer_stats = writer_stats.clone();
        let producer = tokio::task::spawn_blocking(move || -> Result<()> {
            #[cfg(target_os = "linux")]
            producer_stats.producer_tid.store(
                std::fs::read_link("/proc/thread-self")
                    .ok()
                    .and_then(|path| path.file_name()?.to_str()?.parse().ok())
                    .unwrap_or_default(),
                Ordering::Relaxed,
            );
            let result = (|| -> Result<()> {
                let mut encoder = CaptureEncoder::primary(
                    initial.bitrate_kbps,
                    quality.max_fps,
                    quality.max_width,
                    quality.max_height,
                    quality.latency_mode,
                    quality.encoder_preference,
                    producer_stats.clone(),
                )?;
                let mut keyframe_requests = keyframe_requests;
                let mut link_feedback = link_feedback;
                let mut display_selection = display_selection;
                let display_selection_feedback = display_selection_feedback;
                let mut controller = controller;
                let mut fps = initial.fps;
                let mut pacer = DamagePacer::new(fps);
                let mut scale = EncodeScale::new(initial.bitrate_kbps);
                let mut last: Option<Arc<CapturedFrame>> = None;
                let mut last_encoded: Option<Arc<CapturedFrame>> = None;
                let mut forced_keyframe = false;
                let mut waiting_since = Instant::now();
                loop {
                    if producer_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    if encoder
                        .forwarder
                        .as_ref()
                        .is_some_and(|thread| thread.is_finished())
                    {
                        bail!("display capture forwarding thread stopped");
                    }
                    let now = Instant::now();
                    let wait = if last.is_some() {
                        pacer.next_deadline(now, fps).saturating_duration_since(now)
                    } else {
                        Duration::from_millis(50)
                    }
                    .min(Duration::from_millis(50));
                    let wait_started = Instant::now();
                    let fresh = encoder
                        .latest_frame
                        .take_until_stopped(wait, &producer_stopped)?;
                    producer_stats
                        .capture_wait_ns
                        .fetch_add(wait_started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    if producer_stopped.load(Ordering::Acquire) {
                        break;
                    }
                    if keyframe_requests.has_changed().unwrap_or(false) {
                        keyframe_requests.borrow_and_update();
                        forced_keyframe = true;
                    }
                    if delivery_recovery_requests.has_changed().unwrap_or(false) {
                        delivery_recovery_requests.borrow_and_update();
                        forced_keyframe = true;
                    }
                    let now = Instant::now();
                    let send_kbps = producer_stats.send_window.lock().unwrap().kbps(now);
                    producer_stats
                        .send_kbps
                        .store(send_kbps as u32, Ordering::Relaxed);
                    controller.update_sender(send_kbps, pacer.idle(), now);
                    if link_feedback.has_changed().unwrap_or(false)
                        && let Some(sample) = *link_feedback.borrow_and_update()
                        && let Some(decision) = controller.observe(sample)
                    {
                        encoder.apply_bitrate_estimate(
                            decision.bitrate_kbps,
                            quality.max_bitrate_kbps,
                        )?;
                        fps = decision.fps;
                        pacer.set_fps(fps);
                        forced_keyframe |= scale.update(decision.bitrate_kbps);
                        producer_stats
                            .target_kbps
                            .store(decision.bitrate_kbps, Ordering::Relaxed);
                        producer_stats.target_fps.store(fps, Ordering::Relaxed);
                    }
                    let mut switched = false;
                    if display_selection.has_changed().unwrap_or(false) {
                        let display_id = display_selection.borrow_and_update().clone();
                        if !display_id.is_empty() {
                            match encoder.select_display(&display_id) {
                                Ok(()) => {
                                    last = None;
                                    last_encoded = None;
                                    pacer = DamagePacer::new(fps);
                                    forced_keyframe = true;
                                    waiting_since = Instant::now();
                                    switched = true;
                                }
                                Err(error) => {
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
                        }
                    }
                    let mut changed = false;
                    if !switched && let Some(fresh) = fresh {
                        producer_stats.fresh_frames.fetch_add(1, Ordering::Relaxed);
                        let compare_started = Instant::now();
                        changed = last_encoded
                            .as_ref()
                            .is_none_or(|previous| fresh.differs_from(previous));
                        producer_stats.compare_ns.fetch_add(
                            compare_started.elapsed().as_nanos() as u64,
                            Ordering::Relaxed,
                        );
                        if !changed {
                            producer_stats
                                .skipped_unchanged
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        last = Some(fresh);
                    }
                    let Some(captured) = last.as_ref() else {
                        if waiting_since.elapsed() >= Duration::from_secs(5) {
                            bail!("display capture stopped without a frame");
                        }
                        continue;
                    };
                    let keepalive = pacer.idle() && !changed && !forced_keyframe;
                    if pacer.on_frame(Instant::now(), changed, forced_keyframe) == PaceAction::Skip
                    {
                        producer_stats.pacer_skips.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if forced_keyframe {
                        encoder.encoder.force_keyframe();
                        forced_keyframe = false;
                    }
                    let (max_width, max_height) =
                        scale.limits(quality.max_width, quality.max_height);
                    let (frame, _timing) =
                        encoder.encode_captured_frame(captured, max_width, max_height)?;
                    if frame.bytes.is_empty() {
                        continue;
                    }
                    let changed_output = last_encoded
                        .as_ref()
                        .is_none_or(|previous| captured.differs_from(previous));
                    if changed_output {
                        #[cfg(target_os = "linux")]
                        if let Some(started) = producer_stats.damage_started.lock().unwrap().take()
                        {
                            let elapsed = started.elapsed().as_nanos() as u64;
                            producer_stats.first_change_capture_ready_ns.store(
                                captured
                                    .captured_at
                                    .saturating_duration_since(started)
                                    .as_nanos() as u64,
                                Ordering::Relaxed,
                            );
                            producer_stats
                                .first_change_encode_ns
                                .store(_timing.encode.as_nanos() as u64, Ordering::Relaxed);
                            producer_stats
                                .first_change_samples
                                .fetch_add(1, Ordering::Relaxed);
                            producer_stats
                                .first_change_last_ns
                                .store(elapsed, Ordering::Relaxed);
                            producer_stats
                                .first_change_max_ns
                                .fetch_max(elapsed, Ordering::Relaxed);
                        }
                        producer_stats
                            .changed_encodes
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        producer_stats
                            .unchanged_encodes
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    last_encoded = Some(captured.clone());
                    producer_stats
                        .encoded_width
                        .store(frame.width, Ordering::Relaxed);
                    producer_stats
                        .encoded_height
                        .store(frame.height, Ordering::Relaxed);
                    producer_stats
                        .frames_encoded
                        .fetch_add(1, Ordering::Relaxed);
                    if keepalive {
                        producer_stats.keepalives.fetch_add(1, Ordering::Relaxed);
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
        let mut previous_written = None;
        let mut stats_tick = tokio::time::interval(Duration::from_secs(10));
        stats_tick.tick().await;
        let mut delivery_interrupted_since: Option<Instant> = None;
        let mut delivery_error_count = 0_u64;
        let mut recovery_keyframe_requested = false;
        let mut write_result = Ok(());
        loop {
            tokio::select! {
                _ = stats_tick.tick() => writer_stats.log(),
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                frame = encoded_slot.take() => {
                    let Some((frame, _dropped)) = frame else { break };
                    // A replaced encoded frame is a local latency policy, not RTP packet loss.
                    // Passing that count as prev_dropped_packets corrupts packet sequencing.
                    let written_at = Instant::now();
                    let sample = Sample {
                        data: frame.bytes.into(),
                        duration: sample_duration(previous_written, written_at, writer_stats.target_fps.load(Ordering::Relaxed)),
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
                    if result.is_ok() {
                        previous_written = Some(written_at);
                        writer_stats.bytes_sent.fetch_add(sample.data.len() as u64, Ordering::Relaxed);
                        writer_stats.send_window.lock().unwrap().writes.push_back((Instant::now(), sample.data.len()));
                    }
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
        stats,
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
    use openh264::formats::{RgbSliceU8, YUVBuffer, YUVSource};
    use std::time::{Duration, Instant};

    #[test]
    fn send_rate_window_counts_successful_payloads_and_expires_idle_bytes() {
        let now = Instant::now();
        let mut window = super::SendRateWindow::default();
        assert_eq!(window.kbps(now), 0.0);
        window.writes.push_back((now, 125_000));
        window
            .writes
            .push_back((now + Duration::from_secs(1), 125_000));
        assert_eq!(window.kbps(now + Duration::from_secs(1)), 1_000.0);
        assert_eq!(window.kbps(now + Duration::from_millis(2001)), 500.0);
        assert_eq!(window.kbps(now + Duration::from_secs(4)), 0.0);
        assert!(window.writes.is_empty());
    }

    #[tokio::test]
    async fn stopping_a_video_pipeline_waits_for_the_blocking_capture_worker() {
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pipeline = super::VideoPipeline::test_worker(finished.clone());
        assert_eq!(
            pipeline
                .stats
                .frames_encoded
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        pipeline.stop().await;

        assert!(finished.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn encode_scale_bounds_low_rate_keyframes_and_has_recovery_hysteresis() {
        let mut scale = super::EncodeScale::new(2_000);
        assert_eq!(scale.limits(1920, 1080), (1920, 1080));
        assert!(scale.update(1_400));
        assert_eq!(scale.limits(1920, 1080), (960, 540));
        assert!(!scale.update(1_700));
        assert!(scale.update(500));
        assert_eq!(scale.limits(1920, 1080), (480, 270));
        assert!(!scale.update(700));
        assert!(scale.update(900));
        assert!(scale.update(1_900));
        assert_eq!(scale.limits(1920, 1080), (1920, 1080));
        assert_eq!(super::EncodeScale::new(0).limits(0, 1), (2, 2));
    }

    #[test]
    fn sample_durations_use_elapsed_wall_time_and_are_bounded() {
        let now = Instant::now();
        assert_eq!(
            super::sample_duration(None, now, 20),
            Duration::from_millis(50)
        );
        assert_eq!(super::sample_duration(None, now, 0), Duration::from_secs(1));
        assert_eq!(
            super::sample_duration(Some(now), now + Duration::from_secs(2), 60),
            Duration::from_secs(2)
        );
        assert_eq!(
            super::sample_duration(Some(now), now + Duration::from_secs(10), 60),
            Duration::from_secs(5)
        );
        assert_eq!(
            super::sample_duration(Some(now), now, 60),
            Duration::from_millis(1)
        );
        assert_eq!(
            super::sample_duration(Some(now), now - Duration::from_secs(1), 60),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn fresh_capture_wait_does_not_return_cached_frames() {
        let queue = LatestFrame::default();
        let stopped = std::sync::atomic::AtomicBool::new(false);
        assert!(
            queue
                .take_until_stopped(Duration::ZERO, &stopped)
                .unwrap()
                .is_none()
        );
        assert!(queue.replace(xcap::Frame::new(1, 1, vec![1, 0, 0, 255])));
        assert!(
            queue
                .take_until_stopped(Duration::ZERO, &stopped)
                .unwrap()
                .is_some()
        );
        assert!(
            queue
                .take_until_stopped(Duration::ZERO, &stopped)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            queue
                .stats
                .frames_captured
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(queue.take(Duration::ZERO).is_ok());
        queue
            .finished
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(queue.take_until_stopped(Duration::ZERO, &stopped).is_err());
        stopped.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            queue
                .take_until_stopped(Duration::ZERO, &stopped)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn software_bitrate_changes_at_runtime_without_forcing_an_idr() {
        let image =
            xcap::image::RgbaImage::from_pixel(64, 64, xcap::image::Rgba([32, 96, 160, 255]));
        let mut encoder = SoftwareEncoder::new(1_000, 30, LatencyMode::Quality).unwrap();
        assert!(encoder.apply_bitrate_estimate(1_100, 4_000).unwrap());
        let first = encoder.encode(&image).unwrap();
        assert!(annex_b_nal_types(&first.bytes).unwrap().contains(&5));
        for target in [900, 2_000, 1_200] {
            assert!(encoder.apply_bitrate_estimate(target, 4_000).unwrap());
            let mut bitrate = openh264_sys2::SBitrateInfo {
                iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
                iBitrate: 0,
            };
            // SAFETY: GetOption writes the requested bitrate into this matching native structure.
            let result = unsafe {
                encoder.encoder.raw_api().get_option(
                    openh264_sys2::ENCODER_OPTION_BITRATE,
                    std::ptr::from_mut(&mut bitrate).cast(),
                )
            };
            assert_eq!(result, 0);
            assert_eq!(bitrate.iBitrate, target as i32 * 1_000);
            let next = encoder.encode(&image).unwrap();
            let types = annex_b_nal_types(&next.bytes).unwrap();
            assert!(types.contains(&1));
            assert!(!types.contains(&5));
        }
        assert!(!encoder.apply_bitrate_estimate(1_200, 4_000).unwrap());
    }

    #[test]
    fn captured_rgba_view_borrows_pixels_and_validates_layout() {
        let frame = xcap::Frame::new(2, 2, vec![128; 16]);
        let view = super::rgba_view(&frame).unwrap();
        assert_eq!(view.as_raw().as_ptr(), frame.raw.as_ptr());
        assert_eq!(view.dimensions(), (2, 2));
        assert_eq!(view.get_pixel(1, 1).0, [128; 4]);
        assert!(super::rgba_view(&xcap::Frame::new(2, 2, vec![0; 15])).is_err());
        assert_eq!(
            super::rgba_view(&xcap::Frame::new(0, 0, vec![]))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn captured_frame_comparison_checks_pixels_and_dimensions_not_timestamps() {
        let now = Instant::now();
        let previous = super::CapturedFrame {
            frame: xcap::Frame::new(2, 2, vec![0; 16]),
            captured_at: now,
        };
        let mut next = previous.clone();
        next.captured_at += Duration::from_secs(1);
        assert!(!next.differs_from(&previous));
        next.frame.raw[15] = 1;
        assert!(next.differs_from(&previous));
        next.frame.raw.clear();
        assert!(next.differs_from(&previous));
        next.frame.raw = previous.frame.raw.clone();
        next.frame.width = 4;
        next.frame.height = 1;
        assert!(next.differs_from(&previous));
        next.frame.raw.clear();
        next.frame.width = 0;
        next.frame.height = 0;
        assert!(!next.differs_from(&next));
    }

    #[test]
    fn direct_rgba_conversion_matches_packed_rgb_reference() {
        let mut seed = 0x12345678_u32;
        for (width, height) in [(2, 2), (4, 6), (32, 18), (64, 64)] {
            let mut converted = super::I420Frame::new(width, height).unwrap();
            let allocation = converted.bytes.as_ptr();
            for solid in [Some(0), Some(255), None] {
                let rgba: Vec<_> = (0..width * height * 4)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        solid.unwrap_or((seed >> 24) as u8)
                    })
                    .collect();
                let packed: Vec<_> = rgba
                    .chunks_exact(4)
                    .flat_map(|pixel| pixel[..3].iter().copied())
                    .collect();
                let reference =
                    YUVBuffer::from_rgb8_source(RgbSliceU8::new(&packed, (width, height)));
                converted.read_rgba(&rgba).unwrap();
                assert_eq!(converted.bytes.as_ptr(), allocation);
                assert_eq!(converted.strides(), reference.strides());
                assert_eq!(converted.y(), reference.y());
                assert_eq!(converted.u(), reference.u());
                assert_eq!(converted.v(), reference.v());
            }
        }
    }

    #[test]
    fn direct_rgba_conversion_rejects_invalid_layouts() {
        for (width, height) in [(0, 0), (1, 2), (2, 3), (usize::MAX - 1, 2)] {
            assert!(super::I420Frame::new(width, height).is_err());
        }
        let mut converted = super::I420Frame::new(2, 2).unwrap();
        for length in [0, 15, 17] {
            assert!(converted.read_rgba(&vec![0; length]).is_err());
        }
        converted.read_rgba(&[0; 16]).unwrap();
        let original = converted.bytes.clone();
        converted
            .read_rgba(&[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255])
            .unwrap();
        assert_eq!(converted.bytes, original);
    }

    #[test]
    fn idle_reencodes_accept_old_capture_timestamps_but_reject_invalid_pixels() {
        let mut encoder = super::CaptureEncoder {
            recorder: None,
            forwarder: None,
            latest_frame: std::sync::Arc::default(),
            display_id: String::new(),
            encoder: EncoderBackend::Software(Box::new(
                SoftwareEncoder::new(1_000, 30, LatencyMode::Quality).unwrap(),
            )),
        };
        let mut captured = super::CapturedFrame {
            frame: xcap::Frame::new(64, 64, vec![128; 64 * 64 * 4]),
            captured_at: Instant::now() - Duration::from_secs(10),
        };
        let (frame, _) = encoder.encode_captured_frame(&captured, 64, 64).unwrap();
        assert!(!frame.bytes.is_empty());
        encoder.encoder.force_keyframe();
        let (forced, _) = encoder.encode_captured_frame(&captured, 64, 64).unwrap();
        assert!(!forced.bytes.is_empty());
        assert_eq!(
            encoder
                .latest_frame
                .stats
                .frames_captured
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        captured.frame.raw.clear();
        assert!(encoder.encode_captured_frame(&captured, 64, 64).is_err());
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
    fn backend_bitrate_targets_are_clamped_without_additional_hysteresis() {
        assert_eq!(bitrate_target(4_000, 3_500, 250, 8_000), Some(3_500));
        assert_eq!(bitrate_target(4_000, 4_000, 250, 8_000), None);
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
