//! Cross-platform capture and baseline H.264 encoding.
//!
//! Platform release builds enable the `media` feature. The queue between this producer and WebRTC
//! is deliberately capacity one: a remote desktop should drop an obsolete frame under load rather
//! than preserve it and grow latency.

use anyhow::{Context, Result, anyhow, bail};
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, MatrixCoefficients,
    QpRange, RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSource;
use openh264_sys2::{
    ENCODER_OPTION_BITRATE, ENCODER_OPTION_GET_STATISTICS, ENCODER_OPTION_MAX_BITRATE,
    SBitrateInfo, SEncoderStatistics, SPATIAL_LAYER_0, SPATIAL_LAYER_ALL,
};
use xcap::Monitor;

use bytes::Bytes;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use webrtc::media::Sample;
use webrtc::rtp::extension::{HeaderExtension, playout_delay_extension::PlayoutDelayExtension};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

#[cfg(target_os = "linux")]
#[path = "linux_damage.rs"]
mod linux_damage;

#[cfg(feature = "x264")]
#[path = "x264_encoder.rs"]
mod x264_encoder;

#[cfg(target_os = "linux")]
#[path = "media_trace.rs"]
pub(crate) mod trace;

#[cfg(target_os = "macos")]
#[path = "macos_capture.rs"]
mod macos_capture;

#[cfg(target_os = "windows")]
#[path = "media_dxgi.rs"]
mod media_dxgi;

use crate::adaptive::{DamagePacer, IDLE_KEEPALIVE, PaceAction, RateController, frame_changed};
use crate::codec::{VideoCodec, is_keyframe};
use crate::config::QualityConfig;
use crate::config::{EncoderPreference, LatencyMode};

#[cfg(target_os = "windows")]
#[path = "media_resize.rs"]
mod resize;

const VIDEO_DELIVERY_FAILURE_GRACE: Duration = Duration::from_secs(15);
const VIDEO_SAMPLE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const FORCED_IDR_COOLDOWN: Duration = Duration::from_millis(300);
const PERIODIC_IDR_INTERVAL: Duration = Duration::from_secs(20);

// SCTP caps a message at ~64KiB, so large access units are chunked. The compact
// wire header is [u32 seq][u8 flags]; fragmented messages add u16 part/parts
// and timestamped messages add u64 sent_ms. Most delta frames therefore cost
// five header bytes instead of the original seventeen.
const VIDEO_DC_CHUNK: usize = 1200;
// Lossy deltas benefit from MTU-sized application messages, but reliable IDR
// traffic does not: splitting a keyframe into dozens of chunks can keep it
// incomplete longer than the viewer's bounded partial-frame window even on a
// healthy link. Keep keyframe application messages large enough to reassemble
// quickly; SCTP still fragments them on the wire as needed.
const VIDEO_DC_KEYFRAME_CHUNK: usize = 48 * 1024;
const VIDEO_FLAG_KEYFRAME: u8 = 1;
const VIDEO_FLAG_FRAGMENTED: u8 = 2;
const VIDEO_FLAG_TIMESTAMP: u8 = 4;
const VIDEO_FLAG_HEARTBEAT: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoImpairmentPlan {
    SendNow,
    Drop,
    Delay(Duration),
}

/// Deterministic test-only impairment applied at the real SCTP send boundary.
/// Browser loopback otherwise has no packet loss, so bad-network regression
/// tests use environment variables to drop, delay and reorder actual wire
/// messages while keeping the encoder, SCTP transport and browser decoder real.
struct VideoNetworkImpairment {
    loss_ppm: u32,
    reorder_ppm: u32,
    jitter_ms: u64,
    bytes_per_second: u64,
    tokens: f64,
    token_at: Instant,
    random: u64,
}

impl VideoNetworkImpairment {
    fn from_env() -> Option<Self> {
        let parse = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let loss_ppm = parse("NANOCTL_TEST_VIDEO_LOSS_PPM") as u32;
        let reorder_ppm = parse("NANOCTL_TEST_VIDEO_REORDER_PPM") as u32;
        let jitter_ms = parse("NANOCTL_TEST_VIDEO_JITTER_MS");
        let bandwidth_kbps = parse("NANOCTL_TEST_VIDEO_BANDWIDTH_KBPS");
        if loss_ppm == 0 && reorder_ppm == 0 && jitter_ms == 0 && bandwidth_kbps == 0 {
            return None;
        }
        let bytes_per_second = bandwidth_kbps.saturating_mul(1000) / 8;
        Some(Self {
            loss_ppm,
            reorder_ppm,
            jitter_ms,
            bytes_per_second,
            tokens: bytes_per_second as f64 / 4.0,
            token_at: Instant::now(),
            random: 0x9e37_79b9_7f4a_7c15,
        })
    }

    fn next_random(&mut self) -> u32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random >> 32) as u32
    }

    fn chance(&mut self, ppm: u32) -> bool {
        ppm != 0 && self.next_random() % 1_000_000 < ppm
    }

    fn plan(&mut self, len: usize, reliable: bool) -> VideoImpairmentPlan {
        let mut delay = Duration::ZERO;
        if self.bytes_per_second != 0 {
            let now = Instant::now();
            let elapsed = now.saturating_duration_since(self.token_at).as_secs_f64();
            self.token_at = now;
            let capacity = self.bytes_per_second as f64 / 4.0;
            self.tokens = (self.tokens + elapsed * self.bytes_per_second as f64).min(capacity);
            let len = len as f64;
            if self.tokens >= len {
                self.tokens -= len;
            } else if reliable {
                let deficit = len - self.tokens;
                self.tokens = 0.0;
                delay += Duration::from_secs_f64(deficit / self.bytes_per_second as f64);
            } else {
                return VideoImpairmentPlan::Drop;
            }
        }
        // Reliable SCTP retransmission hides ordinary packet loss, so message
        // deletion is only applied to the bounded-retransmit delta lane. The
        // reliable IDR lane still receives bandwidth delay and jitter.
        if !reliable && self.chance(self.loss_ppm) {
            return VideoImpairmentPlan::Drop;
        }
        if self.jitter_ms != 0 {
            delay += Duration::from_millis(self.next_random() as u64 % (self.jitter_ms + 1));
        }
        if !reliable && self.chance(self.reorder_ppm) {
            // A long tail is what exercises the reorder window. Ordinary
            // jitter alone often preserves order on a single-core loopback.
            delay += Duration::from_millis(40 + self.next_random() as u64 % 40);
        }
        if delay.is_zero() {
            VideoImpairmentPlan::SendNow
        } else {
            VideoImpairmentPlan::Delay(delay)
        }
    }
}

fn spawn_delayed_video_send(
    channel: &Arc<webrtc::data_channel::RTCDataChannel>,
    message: &Bytes,
    delay: Duration,
) {
    let channel = channel.clone();
    let message = message.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if let Err(error) = channel.send(&message).await {
            tracing::debug!(%error, "delayed impaired video data channel send failed");
        }
    });
}

async fn send_video_data_message(
    channel: &Arc<webrtc::data_channel::RTCDataChannel>,
    message: &Bytes,
    reliable: bool,
    impairment: &mut Option<VideoNetworkImpairment>,
) -> std::result::Result<bool, webrtc::Error> {
    if let Some(impairment) = impairment {
        match impairment.plan(message.len(), reliable) {
            VideoImpairmentPlan::Drop => return Ok(false),
            VideoImpairmentPlan::Delay(delay) => {
                spawn_delayed_video_send(channel, message, delay);
                return Ok(true);
            }
            VideoImpairmentPlan::SendNow => {}
        }
    }
    channel.send(message).await.map(|_| true)
}

fn video_data_messages(seq: u32, is_keyframe: bool, data: &[u8]) -> Vec<Bytes> {
    let chunk = if is_keyframe {
        VIDEO_DC_KEYFRAME_CHUNK
    } else {
        VIDEO_DC_CHUNK
    };
    let parts = data.len().div_ceil(chunk).max(1) as u16;
    let timestamped = is_keyframe || seq.is_multiple_of(16);
    let sent_ms = if timestamped {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default()
    } else {
        0
    };
    let mut flags = if is_keyframe { VIDEO_FLAG_KEYFRAME } else { 0 };
    if parts > 1 {
        flags |= VIDEO_FLAG_FRAGMENTED;
    }
    if timestamped {
        flags |= VIDEO_FLAG_TIMESTAMP;
    }
    let mut messages = Vec::with_capacity(parts as usize);
    for part in 0..parts {
        let start = part as usize * chunk;
        let end = (start + chunk).min(data.len());
        let mut header = 5 + usize::from(timestamped) * 8;
        if parts > 1 {
            header += 4;
        }
        let mut msg = Vec::with_capacity(end - start + header);
        msg.extend_from_slice(&seq.to_le_bytes());
        msg.push(flags);
        if parts > 1 {
            msg.extend_from_slice(&part.to_le_bytes());
            msg.extend_from_slice(&parts.to_le_bytes());
        }
        if timestamped {
            msg.extend_from_slice(&sent_ms.to_le_bytes());
        }
        msg.extend_from_slice(&data[start..end]);
        messages.push(Bytes::from(msg));
    }
    messages
}

fn video_heartbeat_message(next_seq: u32) -> Bytes {
    let mut msg = Vec::with_capacity(5);
    msg.extend_from_slice(&next_seq.to_le_bytes());
    msg.push(VIDEO_FLAG_HEARTBEAT);
    Bytes::from(msg)
}

fn video_playout_delay(mode: LatencyMode) -> HeaderExtension {
    HeaderExtension::PlayoutDelay(PlayoutDelayExtension {
        min_delay: 0,
        max_delay: if matches!(mode, LatencyMode::Quality) {
            10
        } else {
            0
        },
    })
}

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
    pub capture_ns: AtomicU64,
    pub send_ns: AtomicU64,
    pub sample_writes: AtomicU64,
    pub encode_attempts: AtomicU64,
    pub empty_encodes: AtomicU64,
    pub pacer_skips: AtomicU64,
    pub backpressure_skips: AtomicU64,
    pub dc_backpressure_events: AtomicU64,
    pub queue_ns: AtomicU64,
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
            capture_ns = self.capture_ns.load(Ordering::Relaxed),
            compare_ns = self.compare_ns.load(Ordering::Relaxed),
            preprocess_ns = self.preprocess_ns.load(Ordering::Relaxed),
            color_convert_ns = self.color_convert_ns.load(Ordering::Relaxed),
            backend_ns = self.backend_ns.load(Ordering::Relaxed),
            encode_attempts = self.encode_attempts.load(Ordering::Relaxed),
            empty_encodes = self.empty_encodes.load(Ordering::Relaxed),
            capture_wait_ns = self.capture_wait_ns.load(Ordering::Relaxed),
            send_ns = self.send_ns.load(Ordering::Relaxed),
            sample_writes = self.sample_writes.load(Ordering::Relaxed),
            fresh_frames = self.fresh_frames.load(Ordering::Relaxed),
            pacer_skips = self.pacer_skips.load(Ordering::Relaxed),
            backpressure_skips = self.backpressure_skips.load(Ordering::Relaxed),
            dc_backpressure_events = self.dc_backpressure_events.load(Ordering::Relaxed),
            queue_ns = self.queue_ns.load(Ordering::Relaxed),
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

    /// Update the divisor and report whether the clamped encode size
    /// actually changed — a divisor flip that the floor absorbs needs
    /// no keyframe and no encoder resize.
    fn changed_limits(&mut self, kbps: u32, width: u32, height: u32) -> bool {
        let before = self.limits(width, height);
        self.update(kbps);
        self.limits(width, height) != before
    }

    fn limits(&self, width: u32, height: u32) -> (u32, u32) {
        // Low-bitrate links cannot carry enough bits per 1080p pixel: keeping
        // the source resolution there turns motion into macroblock loss and
        // reference-chain churn. The divisor is already bitrate-gated with
        // hysteresis, so use it directly and never upscale tiny sources.
        let divisor = self.0.max(1);
        ((width / divisor).max(2), (height / divisor).max(2))
    }
}

fn viewport_scaled_rate(bitrate_kbps: u32, ceiling_kbps: u32, pixel_ratio: f64) -> (u32, u32) {
    if pixel_ratio >= 1.0 {
        return (bitrate_kbps, ceiling_kbps);
    }
    // Bitrate needs fall off more slowly than pixel count: the smaller encode has
    // less redundant area and needs more bits per output pixel to remain transparent
    // at the physical size the viewer actually renders. The 0.8 exponent was
    // measured indistinguishable from sqrt scaling at the viewport while using
    // 22-34% fewer bytes.
    let rate_scale = pixel_ratio.powf(0.8);
    (
        (f64::from(bitrate_kbps) * rate_scale).round().max(1.0) as u32,
        (f64::from(ceiling_kbps) * rate_scale).round().max(1.0) as u32,
    )
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

/// Registers the calling thread with MMCSS "Pro Audio" so the multimedia
/// scheduler grants it low-latency timeslices - the same trick Sunshine uses
/// for capture/encode threads on Windows. Returns a guard reverting on drop.
#[cfg(windows)]
pub(crate) fn register_mmcss() -> Option<MmcssGuard> {
    use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    let mut index = 0u32;
    let handle =
        unsafe { AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut index) }.ok();
    let handle = handle?;
    Some(MmcssGuard(handle))
}

#[cfg(windows)]
pub(crate) struct MmcssGuard(windows::Win32::Foundation::HANDLE);

// The MMCSS handle is an opaque scheduler registration token. Ownership can
// move with the callback/stream that registered it; only Drop consumes it.
#[cfg(windows)]
unsafe impl Send for MmcssGuard {}

#[cfg(windows)]
impl Drop for MmcssGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(self.0);
        }
    }
}

/// Keeps Windows awake while an active stream exists. Without this, a host
/// whose display power-save fires mid-session can stop producing meaningful
/// frames exactly when the remote user is still watching.
#[cfg(windows)]
struct DisplayWakeGuard;

#[cfg(windows)]
impl DisplayWakeGuard {
    fn acquire() -> Self {
        use windows::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, SetThreadExecutionState,
        };
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED);
        }
        Self
    }
}

#[cfg(windows)]
impl Drop for DisplayWakeGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}

/// Sunshine gives streaming latency-sensitive scheduling while a session is
/// active: the compositor uses MMCSS and this process runs at high priority.
/// Keep a small reference count here rather than assuming exclusive ownership:
/// concurrent media producers must not let the first stream teardown demote
/// the process while another stream is still active.
#[cfg(windows)]
static STREAMING_PRIORITY_STATE: std::sync::Mutex<(usize, Option<u32>, bool)> =
    std::sync::Mutex::new((0, None, false));

/// Windows Wi-Fi scanning otherwise adds exactly the kind of jitter a game
/// stream feels. Sunshine enables the documented media-streaming mode on each
/// connected WLAN interface while streaming. Load wlanapi dynamically because
/// Windows Server often does not install the Wireless LAN Service at all; a
/// hard import would make the whole agent fail to start on wired hosts.
#[cfg(windows)]
type WlanOpen = unsafe extern "system" fn(
    u32,
    *const core::ffi::c_void,
    *mut u32,
    *mut windows::Win32::Foundation::HANDLE,
) -> u32;
#[cfg(windows)]
type WlanClose =
    unsafe extern "system" fn(windows::Win32::Foundation::HANDLE, *const core::ffi::c_void) -> u32;
#[cfg(windows)]
type WlanEnum = unsafe extern "system" fn(
    windows::Win32::Foundation::HANDLE,
    *const core::ffi::c_void,
    *mut *mut windows::Win32::NetworkManagement::WiFi::WLAN_INTERFACE_INFO_LIST,
) -> u32;
#[cfg(windows)]
type WlanFree = unsafe extern "system" fn(*const core::ffi::c_void);
#[cfg(windows)]
type WlanSet = unsafe extern "system" fn(
    windows::Win32::Foundation::HANDLE,
    *const windows::core::GUID,
    windows::Win32::NetworkManagement::WiFi::WLAN_INTF_OPCODE,
    u32,
    *const core::ffi::c_void,
    *const core::ffi::c_void,
) -> u32;

#[cfg(windows)]
struct WlanStreamingGuard {
    handle: windows::Win32::Foundation::HANDLE,
    close: WlanClose,
    _library: libloading::Library,
}

#[cfg(windows)]
impl WlanStreamingGuard {
    fn acquire() -> Option<Self> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::NetworkManagement::WiFi::{
            WLAN_INTERFACE_INFO_LIST, wlan_interface_state_connected,
            wlan_intf_opcode_media_streaming_mode,
        };
        unsafe {
            let library = libloading::Library::new("wlanapi.dll").ok()?;
            let open = *library.get::<WlanOpen>(b"WlanOpenHandle").ok()?;
            let close = *library.get::<WlanClose>(b"WlanCloseHandle").ok()?;
            let enumerate = *library.get::<WlanEnum>(b"WlanEnumInterfaces").ok()?;
            let free = *library.get::<WlanFree>(b"WlanFreeMemory").ok()?;
            let set = *library.get::<WlanSet>(b"WlanSetInterface").ok()?;
            let mut negotiated = 0u32;
            let mut handle = HANDLE::default();
            if open(2, core::ptr::null(), &mut negotiated, &mut handle) != 0 {
                return None;
            }
            let mut list = std::ptr::null_mut::<WLAN_INTERFACE_INFO_LIST>();
            if enumerate(handle, core::ptr::null(), &mut list) == 0 && !list.is_null() {
                let count = (*list).dwNumberOfItems as usize;
                let interfaces = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), count);
                let enabled = 1i32;
                for interface in interfaces {
                    if interface.isState != wlan_interface_state_connected {
                        continue;
                    }
                    let status = set(
                        handle,
                        &interface.InterfaceGuid,
                        wlan_intf_opcode_media_streaming_mode,
                        4,
                        &enabled as *const i32 as _,
                        core::ptr::null(),
                    );
                    if status != 0 {
                        tracing::warn!(status, "Windows Wi-Fi media-streaming mode failed");
                    }
                }
                free(list as _);
            }
            Some(Self {
                handle,
                close,
                _library: library,
            })
        }
    }
}

#[cfg(windows)]
impl Drop for WlanStreamingGuard {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle, core::ptr::null());
        }
    }
}

#[cfg(windows)]
struct StreamingPriorityGuard;

#[cfg(windows)]
impl StreamingPriorityGuard {
    fn acquire() -> Self {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, HIGH_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS,
            SetPriorityClass,
        };
        let mut state = STREAMING_PRIORITY_STATE.lock().unwrap();
        state.0 += 1;
        if state.0 == 1 {
            match unsafe { windows::Win32::Graphics::Dwm::DwmEnableMMCSS(true) } {
                Ok(()) => state.2 = true,
                Err(error) => tracing::warn!(?error, "Windows DWM MMCSS scheduling boost failed"),
            }
            unsafe {
                let process = GetCurrentProcess();
                let previous = GetPriorityClass(process);
                if previous == 0 {
                    tracing::warn!("Windows streaming process priority query failed");
                } else if previous != HIGH_PRIORITY_CLASS.0 && previous != REALTIME_PRIORITY_CLASS.0
                {
                    match SetPriorityClass(process, HIGH_PRIORITY_CLASS) {
                        Ok(()) => state.1 = Some(previous),
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "Windows streaming process priority boost failed"
                            )
                        }
                    }
                }
            }
        }
        Self
    }
}

#[cfg(windows)]
impl Drop for StreamingPriorityGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, HIGH_PRIORITY_CLASS, SetPriorityClass,
        };
        let mut state = STREAMING_PRIORITY_STATE.lock().unwrap();
        if state.0 > 0 {
            state.0 -= 1;
        }
        if state.0 != 0 {
            return;
        }
        if state.2 {
            state.2 = false;
            if let Err(error) = unsafe { windows::Win32::Graphics::Dwm::DwmEnableMMCSS(false) } {
                tracing::warn!(?error, "Windows DWM MMCSS scheduling restore failed");
            }
        }
        if let Some(previous) = state.1.take() {
            unsafe {
                let process = GetCurrentProcess();
                if GetPriorityClass(process) == HIGH_PRIORITY_CLASS.0
                    && let Err(error) = SetPriorityClass(
                        process,
                        windows::Win32::System::Threading::PROCESS_CREATION_FLAGS(previous),
                    )
                {
                    tracing::warn!(?error, "Windows streaming process priority restore failed");
                }
            }
        }
    }
}

/// Stash of pixel buffers released by fully-dropped `CapturedFrame`s, each
/// tagged with the emit generation it last carried.
type RecycledFrames = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

/// A backend-reported rectangle whose pixels may differ from the previously
/// encoded desktop. Coordinates are clipped, exclusive-end source pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DirtyRect {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

fn compact_dirty_rects(rects: &mut Vec<DirtyRect>) {
    if rects.len() <= 16 {
        return;
    }
    let left = rects.iter().map(|rect| rect.left).min().unwrap_or(0);
    let top = rects.iter().map(|rect| rect.top).min().unwrap_or(0);
    let right = rects.iter().map(|rect| rect.right).max().unwrap_or(0);
    let bottom = rects.iter().map(|rect| rect.bottom).max().unwrap_or(0);
    *rects = vec![DirtyRect {
        left,
        top,
        right,
        bottom,
    }];
}

#[derive(Default)]
struct LatestFrame {
    slot: Mutex<Option<Arc<CapturedFrame>>>,
    last: Mutex<Option<Arc<CapturedFrame>>>,
    ready: Condvar,
    stopped: AtomicBool,
    finished: AtomicBool,
    stats: Arc<PipelineStats>,
    /// Pixel buffers released by fully-dropped `CapturedFrame`s, tagged with
    /// the emit generation they last carried. The capture thread reuses them
    /// so per-frame `Frame` emission does not clone the full frame.
    recycled: RecycledFrames,
}

#[derive(Clone)]
struct CapturedFrame {
    frame: xcap::Frame,
    format: PixelFormat,
    captured_at: Instant,
    /// `Some(true)`/`Some(false)` when the capture backend itself knows whether
    /// the pixels changed (e.g. DXGI dirty rects) — the expensive full-frame
    /// memcmp is then skipped. `None` falls back to byte comparison.
    backend_changed: Option<bool>,
    /// Monotonic emit counter from the capture backend (0 = unknown).
    emit_gen: u64,
    /// Dirty regions reported since the preceding capture emission, including
    /// any unseen frame this one replaced in the capture slot. `None` means
    /// the backend cannot prove which macroblocks are unchanged.
    dirty_rects: Option<Arc<Vec<DirtyRect>>>,
    /// Sink this frame returns its pixel buffer to on drop, when it came from
    /// a recycling backend.
    recycle: Option<RecycledFrames>,
}

impl Drop for CapturedFrame {
    fn drop(&mut self) {
        let Some(recycle) = &self.recycle else {
            return;
        };
        if let Ok(mut stash) = recycle.lock()
            && stash.len() < 3
        {
            stash.push((self.emit_gen, std::mem::take(&mut self.frame.raw)));
        }
    }
}

impl CapturedFrame {
    fn differs_from(&self, previous: &Self) -> bool {
        if self.frame.width != previous.frame.width
            || self.frame.height != previous.frame.height
            || self.format != previous.format
        {
            return true;
        }
        if let Some(changed) = self.backend_changed {
            return changed;
        }
        frame_changed(&previous.frame.raw, &self.frame.raw)
    }
}

impl LatestFrame {
    fn replace(&self, frame: xcap::Frame) -> bool {
        self.replace_hint(frame, PixelFormat::Rgba, None, 0, None)
    }

    /// Drains the recycled pixel-buffer stash — (emit generation, buffer)
    /// pairs released by fully-dropped `CapturedFrame`s.
    #[cfg(target_os = "windows")]
    fn take_recycled(&self) -> Vec<(u64, Vec<u8>)> {
        self.recycled
            .lock()
            .map(|mut stash| std::mem::take(&mut *stash))
            .unwrap_or_default()
    }

    fn replace_hint(
        &self,
        frame: xcap::Frame,
        format: PixelFormat,
        backend_changed: Option<bool>,
        emit_gen: u64,
        dirty_rects: Option<Vec<DirtyRect>>,
    ) -> bool {
        let Ok(mut slot) = self.slot.lock() else {
            return false;
        };
        let dirty_rects = dirty_rects.map(|mut rects| {
            // Frames dropped while the encoder was busy still contributed pixels
            // to this full-frame emission, so their damage must stay in the hint.
            if let Some(previous) = slot.as_ref().and_then(|previous| {
                (previous.frame.width == frame.width && previous.frame.height == frame.height)
                    .then(|| previous.dirty_rects.clone())
                    .flatten()
            }) {
                rects.extend(previous.iter().copied());
            }
            compact_dirty_rects(&mut rects);
            Arc::new(rects)
        });
        let captured = Arc::new(CapturedFrame {
            frame,
            format,
            captured_at: Instant::now(),
            backend_changed,
            emit_gen,
            dirty_rects,
            recycle: (emit_gen > 0).then(|| self.recycled.clone()),
        });
        if let Ok(mut last) = self.last.lock() {
            *last = Some(captured.clone());
        }
        *slot = Some(captured);
        self.stats.frames_captured.fetch_add(1, Ordering::Relaxed);
        self.ready.notify_one();
        true
    }

    fn take(&self, timeout: Duration) -> Result<(Arc<CapturedFrame>, bool)> {
        let slot = self
            .slot
            .lock()
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        if self.finished.load(Ordering::Acquire) {
            bail!("display capture stopped");
        }
        if slot.is_none()
            && let Some(last) = self
                .last
                .lock()
                .map_err(|_| anyhow!("display capture queue was poisoned"))?
                .clone()
        {
            return Ok((last, false));
        }
        let (mut slot, timeout) = self
            .ready
            .wait_timeout_while(slot, timeout, |frame| {
                frame.is_none() && !self.finished.load(Ordering::Acquire)
            })
            .map_err(|_| anyhow!("display capture queue was poisoned"))?;
        if timeout.timed_out() && slot.is_none() {
            return self
                .last
                .lock()
                .map_err(|_| anyhow!("display capture queue was poisoned"))?
                .clone()
                .map(|last| (last, false))
                .context("display capture stopped without a frame");
        }
        slot.take()
            .map(|frame| (frame, true))
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
                bail!("display capture stopped");
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
    recorder: Option<CaptureRecorder>,
    forwarder: Option<std::thread::JoinHandle<()>>,
    latest_frame: Arc<LatestFrame>,
    display_id: String,
    encoder: EncoderBackend,
    /// In-process decode-and-compare probe used by `media-smoke` to report PSNR.
    psnr: Option<PsnrProbe>,
    #[cfg(target_os = "windows")]
    resizer: resize::RgbaResizer,
}

enum CaptureRecorder {
    Xcap(xcap::VideoRecorder),
    #[cfg(target_os = "macos")]
    ScreenCaptureKit(macos_capture::ScreenRecorder),
}

impl CaptureRecorder {
    fn stop(self) {
        match self {
            Self::Xcap(recorder) => {
                let _ = recorder.stop();
            }
            #[cfg(target_os = "macos")]
            Self::ScreenCaptureKit(recorder) => drop(recorder),
        }
    }
}

pub(crate) use crate::pixel_format::PixelFormat;

pub(crate) type RgbaFrame<'a> = xcap::image::ImageBuffer<xcap::image::Rgba<u8>, &'a [u8]>;

pub(crate) trait FrameEncoder {
    fn force_keyframe(&mut self);
    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()>;
    /// Whether the current bitrate target is below the session ceiling because
    /// network probing has not proved more egress. Encoders with expensive
    /// compression knobs use this to avoid mistaking a network cap for content
    /// that needs a faster, less efficient parameter tier.
    fn set_network_constrained(&mut self, _constrained: bool) {}
    fn color_convert_ns(&self) -> u64 {
        0
    }
    fn encode(&mut self, image: &RgbaFrame<'_>) -> Result<EncodedFrame>;
    /// Encodes a frame whose packed channel order is `format`. Encoders without
    /// native support pay a correctness-preserving swizzle here; hot Windows
    /// backends override this to consume native DXGI BGRA directly.
    fn encode_format(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
    ) -> Result<EncodedFrame> {
        if format == PixelFormat::Rgba {
            return self.encode(image);
        }
        let mut rgba = Vec::with_capacity(image.as_raw().len());
        for pixel in image.as_raw().chunks_exact(4) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        let converted = RgbaFrame::from_raw(image.width(), image.height(), rgba.as_slice())
            .context("invalid BGRA frame dimensions")?;
        self.encode(&converted)
    }
    /// Encodes with a per-macroblock `1 = provably constant, 0 = inspect`
    /// hint map in row-major 16x16 macroblock order. Encoders without a fast
    /// constant-block path ignore it.
    fn encode_format_with_constant_mb(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
        _constant_mb: Option<&[u8]>,
    ) -> Result<EncodedFrame> {
        self.encode_format(image, format)
    }
    /// (tier, switches) for encoders with content-driven parameter negotiation.
    fn adapt_state(&self) -> Option<(u8, u32)> {
        None
    }
    fn software_backend(&self) -> (&'static str, &'static str) {
        ("OpenH264", "software-openh264")
    }
}

#[derive(Clone, Copy)]
struct EncoderSettings {
    codec: VideoCodec,
    h264_high: bool,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
}

type OpenEncoder = fn(EncoderSettings, (u32, u32)) -> Result<Box<dyn FrameEncoder>>;

struct HardwareBackend {
    id: &'static str,
    name: &'static str,
    codecs: &'static [VideoCodec],
    open: OpenEncoder,
}

macro_rules! hardware_backend {
    ($id:literal, $name:literal, $encoder:path, codecs: [$($codec:ident),+]) => {
        HardwareBackend {
            id: $id,
            name: $name,
            codecs: &[$(VideoCodec::$codec),+],
            open: |settings, dimensions| {
                Ok(Box::new(<$encoder>::with_codec(
                    settings.codec,
                    settings.h264_high,
                    settings.bitrate_kbps,
                    settings.max_fps,
                    settings.latency_mode,
                    dimensions,
                )?))
            },
        }
    };
    ($id:literal, $name:literal, $encoder:path) => {
        HardwareBackend {
            id: $id,
            name: $name,
            codecs: &[VideoCodec::H264],
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
    hardware_backend!(
        "nvenc",
        "NVENC",
        crate::nvenc_encoder::NvencEncoder,
        codecs: [H264, Hevc, Av1]
    ),
    #[cfg(target_os = "macos")]
    hardware_backend!(
        "videotoolbox",
        "VideoToolbox",
        crate::macos_encoder::MacOsEncoder,
        codecs: [H264]
    ),
    #[cfg(target_os = "windows")]
    hardware_backend!(
        "media-foundation",
        "Media Foundation hardware MFT",
        crate::windows_encoder::WindowsEncoder,
        codecs: [H264, Hevc, Av1]
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
    network_constrained: bool,
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

    #[cfg(test)]
    fn read_rgba(&mut self, rgba: &[u8]) -> Result<()> {
        self.read_pixels(rgba, PixelFormat::Rgba)
    }

    fn read_pixels(&mut self, pixels_in: &[u8], format: PixelFormat) -> Result<()> {
        self.read_pixels_rect(pixels_in, format, 0, 0, self.width, self.height)
    }

    /// Converts one even-aligned rectangle while preserving the rest of the
    /// persistent I420 image. H.264 4:2:0 chroma is computed independently
    /// per 2x2 pixel block, so this is bit-identical to a full-frame conversion
    /// when every changed pixel is inside the rectangle set.
    fn read_pixels_rect(
        &mut self,
        pixels_in: &[u8],
        format: PixelFormat,
        left: usize,
        top: usize,
        right: usize,
        bottom: usize,
    ) -> Result<()> {
        let pixels = self.width * self.height;
        if Some(pixels_in.len()) != pixels.checked_mul(4) {
            bail!("pixel buffer does not match I420 dimensions");
        }
        if !left.is_multiple_of(2)
            || !top.is_multiple_of(2)
            || !right.is_multiple_of(2)
            || !bottom.is_multiple_of(2)
            || right > self.width
            || bottom > self.height
            || left >= right
            || top >= bottom
        {
            bail!("invalid partial I420 conversion rectangle");
        }
        // `yuv` walks stride-sized rows and therefore needs the padding after
        // the final row to exist in the slice, even though it converts only
        // `right - left` pixels. For a rectangle ending at the bottom edge
        // that padding is outside the frame unless the slice starts at the
        // left edge. Widening the rectangle leftward is exact: converting
        // unchanged extra pixels produces the same I420 bytes.
        let left = if bottom == self.height { 0 } else { left };
        let (y, uv) = self.bytes.split_at_mut(pixels);
        let (u, v) = uv.split_at_mut(pixels / 4);
        let width = right - left;
        let height = bottom - top;
        let chroma_left = left / 2;
        let chroma_top = top / 2;
        let chroma_stride = self.width / 2;
        let y_offset = top * self.width + left;
        let chroma_offset = chroma_top * chroma_stride + chroma_left;
        let input_offset = (top * self.width + left) * 4;
        // `yuv` iterates whole stride-sized rows. Cap each slice at the end of
        // the rectangle's last row; passing the rest of the frame silently
        // converts those trailing rows too and erases the partial-conversion
        // win. Row padding itself is present but not converted because the
        // explicit rectangle width is narrower than the stride.
        let y_len = height * self.width;
        let chroma_height = height / 2;
        let chroma_len = chroma_height * chroma_stride;
        let mut planar = yuv::YuvPlanarImageMut {
            y_plane: yuv::BufferStoreMut::Borrowed(&mut y[y_offset..y_offset + y_len]),
            y_stride: self.width as u32,
            u_plane: yuv::BufferStoreMut::Borrowed(
                &mut u[chroma_offset..chroma_offset + chroma_len],
            ),
            u_stride: chroma_stride as u32,
            v_plane: yuv::BufferStoreMut::Borrowed(
                &mut v[chroma_offset..chroma_offset + chroma_len],
            ),
            v_stride: chroma_stride as u32,
            width: width as u32,
            height: height as u32,
        };
        let stride = (self.width * 4) as u32;
        let input_len = height * self.width * 4;
        let input = &pixels_in[input_offset..input_offset + input_len];
        let result = match format {
            PixelFormat::Rgba => yuv::rgba_to_yuv420(
                &mut planar,
                input,
                stride,
                yuv::YuvRange::Limited,
                yuv::YuvStandardMatrix::Bt601,
                yuv::YuvConversionMode::Balanced,
            ),
            PixelFormat::Bgra => yuv::bgra_to_yuv420(
                &mut planar,
                input,
                stride,
                yuv::YuvRange::Limited,
                yuv::YuvStandardMatrix::Bt601,
                yuv::YuvConversionMode::Balanced,
            ),
        };
        result.map_err(|error| anyhow!("pixel input to I420 conversion failed: {error}"))?;
        Ok(())
    }

    /// Converts only macroblocks marked dirty (`0`) in a row-major 16x16
    /// constant map. Fragmented or large damage falls back to the exact
    /// full-frame path; an empty damage set is a conversion no-op.
    fn read_pixels_constant_mb(
        &mut self,
        pixels_in: &[u8],
        format: PixelFormat,
        constant_mb: &[u8],
    ) -> Result<()> {
        let mb_width = self.width.div_ceil(16);
        let mb_height = self.height.div_ceil(16);
        if constant_mb.len() != mb_width * mb_height {
            bail!("constant macroblock map does not match I420 dimensions");
        }
        let dirty = constant_mb.iter().filter(|value| **value == 0).count();
        if dirty == 0 {
            return Ok(());
        }
        if dirty * 10 >= constant_mb.len() * 3 {
            return self.read_pixels(pixels_in, format);
        }
        let mut rects: Vec<(usize, usize, usize, usize)> = Vec::new();
        for mb_y in 0..mb_height {
            let mut x = 0;
            while x < mb_width {
                if constant_mb[mb_y * mb_width + x] != 0 {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < mb_width && constant_mb[mb_y * mb_width + x] == 0 {
                    x += 1;
                }
                if let Some(rect) = rects
                    .iter_mut()
                    .find(|rect| rect.0 == start && rect.1 == x && rect.3 == mb_y)
                {
                    rect.3 = mb_y + 1;
                } else {
                    rects.push((start, x, mb_y, mb_y + 1));
                }
            }
        }
        for (x0, x1, y0, y1) in rects {
            self.read_pixels_rect(
                pixels_in,
                format,
                (x0 * 16).min(self.width),
                (y0 * 16).min(self.height),
                (x1 * 16).min(self.width),
                (y1 * 16).min(self.height),
            )?;
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

/// Decode-and-compare probe for `media-smoke`: each encoded access unit goes back through
/// OpenH264's decoder and the result is compared (Y+U+V PSNR, dB) against the exact I420
/// the encoder consumed. Opt-in so production sessions pay nothing.
struct PsnrProbe {
    decoder: Decoder,
    scratch: Option<I420Frame>,
    pub decoded: u64,
    pub samples: Vec<f64>,
}

impl PsnrProbe {
    fn new() -> Result<Self> {
        Ok(Self {
            decoder: Decoder::new().context("H.264 decoder is unavailable")?,
            scratch: None,
            decoded: 0,
            samples: Vec::new(),
        })
    }

    fn observe(&mut self, image: &RgbaFrame<'_>, format: PixelFormat, access_unit: &[u8]) {
        if let Err(error) = self.observe_inner(image, format, access_unit) {
            tracing::debug!(%error, "psnr probe skipped a frame");
        }
    }

    fn observe_inner(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
        access_unit: &[u8],
    ) -> Result<()> {
        let dimensions = (image.width() as usize, image.height() as usize);
        if self
            .scratch
            .as_ref()
            .is_none_or(|frame| (frame.width, frame.height) != dimensions)
        {
            self.scratch = Some(I420Frame::new(dimensions.0, dimensions.1)?);
        }
        let source = self
            .scratch
            .as_mut()
            .context("I420 buffer is unavailable")?;
        source.read_pixels(image.as_raw(), format)?;
        let Some(decoded) = self
            .decoder
            .decode(access_unit)
            .context("H.264 decode failed")?
        else {
            return Ok(());
        };
        if decoded.dimensions() != dimensions {
            return Ok(());
        }
        self.decoded += 1;
        self.samples.push(i420_psnr(source, &decoded));
        Ok(())
    }
}

fn plane_mse(
    source: &[u8],
    source_stride: usize,
    decoded: &[u8],
    decoded_stride: usize,
    width: usize,
    height: usize,
) -> f64 {
    let mut sum = 0f64;
    for row in 0..height {
        let source_row = &source[row * source_stride..row * source_stride + width];
        let decoded_row = &decoded[row * decoded_stride..row * decoded_stride + width];
        for (&a, &b) in source_row.iter().zip(decoded_row.iter()) {
            let d = f64::from(a) - f64::from(b);
            sum += d * d;
        }
    }
    sum
}

/// PSNR (dB) over all three I420 planes; identical frames report the 99dB cap.
fn i420_psnr(source: &I420Frame, decoded: &impl YUVSource) -> f64 {
    let (width, height) = source.dimensions();
    let (sy, su, sv) = decoded.strides();
    let pixels = (width * height) as f64;
    let mse_sum = plane_mse(source.y(), width, decoded.y(), sy, width, height)
        + plane_mse(
            source.u(),
            width / 2,
            decoded.u(),
            su,
            width / 2,
            height / 2,
        )
        + plane_mse(
            source.v(),
            width / 2,
            decoded.v(),
            sv,
            width / 2,
            height / 2,
        );
    let mse = mse_sum / (pixels * 1.5);
    if mse <= 0.0 {
        99.0
    } else {
        10.0 * (255.0_f64 * 255.0 / mse).log10()
    }
}

impl CaptureEncoder {
    #[allow(clippy::too_many_arguments)]
    pub fn primary(
        max_bitrate_kbps: u32,
        max_fps: u16,
        max_width: u32,
        max_height: u32,
        latency_mode: LatencyMode,
        encoder_preference: EncoderPreference,
        codec: VideoCodec,
        h264_high: bool,
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
        let encoder = EncoderBackend::new_for_codec(
            codec,
            h264_high,
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
            forwarder,
            latest_frame,
            display_id,
            encoder,
            psnr: None,
            #[cfg(target_os = "windows")]
            resizer: resize::RgbaResizer::default(),
        })
    }

    pub fn select_display(&mut self, display_id: &str) -> Result<()> {
        if display_id == self.display_id {
            return Ok(());
        }
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
            old_recorder.stop();
        }
        if let Some(old_forwarder) = self.forwarder.take() {
            let _ = old_forwarder.join();
        }
        self.recorder = recorder;
        self.forwarder = forwarder;
        self.latest_frame = latest_frame;
        self.display_id = display_id.to_owned();
        self.encoder.force_keyframe();
        Ok(())
    }

    fn current_display_id(&self) -> &str {
        &self.display_id
    }

    fn adapt_state(&self) -> Option<(u8, u32)> {
        self.encoder.adapt_state()
    }

    pub fn enable_psnr_probe(&mut self) -> Result<()> {
        self.psnr = Some(PsnrProbe::new()?);
        Ok(())
    }

    pub fn psnr_report(&self) -> Option<(u64, &[f64])> {
        self.psnr
            .as_ref()
            .map(|probe| (probe.decoded, probe.samples.as_slice()))
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
    ) -> Result<(EncodedFrame, TimingSample, bool)> {
        let (captured, fresh) = self.latest_frame.take(Duration::from_secs(5))?;
        let damage = if fresh {
            captured.dirty_rects.as_deref().map(Vec::as_slice)
        } else {
            // Timed-out smoke re-encodes repeat the exact pixels of the prior
            // encode, so every macroblock is provably constant.
            Some(&[][..])
        };
        let (encoded, timing) =
            self.encode_captured_frame_with_damage(&captured, max_width, max_height, damage)?;
        Ok((encoded, timing, fresh))
    }

    #[cfg(test)]
    fn encode_captured_frame(
        &mut self,
        captured: &CapturedFrame,
        max_width: u32,
        max_height: u32,
    ) -> Result<(EncodedFrame, TimingSample)> {
        self.encode_captured_frame_with_damage(
            captured,
            max_width,
            max_height,
            captured.dirty_rects.as_deref().map(Vec::as_slice),
        )
    }

    fn encode_captured_frame_with_damage(
        &mut self,
        captured: &CapturedFrame,
        max_width: u32,
        max_height: u32,
        dirty_rects: Option<&[DirtyRect]>,
    ) -> Result<(EncodedFrame, TimingSample)> {
        let CapturedFrame {
            frame,
            format,
            captured_at,
            ..
        } = captured;
        #[cfg(target_os = "linux")]
        let trace_started = trace::now();
        let preprocess_started = Instant::now();
        self.latest_frame.stats.queue_ns.fetch_add(
            preprocess_started
                .saturating_duration_since(*captured_at)
                .as_nanos() as u64,
            Ordering::Relaxed,
        );
        let source = rgba_view(frame)?;
        let (source_width, source_height) = source.dimensions();
        let (target_width, target_height) =
            fit_dimensions(source_width, source_height, max_width, max_height);
        let resized;
        let image = if target_width == source_width && target_height == source_height {
            source
        } else {
            resized = {
                #[cfg(target_os = "windows")]
                {
                    self.resizer.resize(
                        source.as_raw(),
                        source_width,
                        source_height,
                        target_width,
                        target_height,
                    )?
                }
                #[cfg(not(target_os = "windows"))]
                {
                    xcap::image::imageops::resize(
                        &source,
                        target_width,
                        target_height,
                        xcap::image::imageops::FilterType::Triangle,
                    )
                }
            };
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
        #[cfg(target_os = "windows")]
        {
            static DUMPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            if let Ok(path) = std::env::var("NANOCTL_DUMP_FRAME")
                && !path.is_empty()
            {
                let n = DUMPED.fetch_add(1, Ordering::Relaxed);
                if n < 3 {
                    let p = path.replace(".png", &format!("{n}.png"));
                    if *format == PixelFormat::Bgra {
                        let mut rgba = image.as_raw().to_vec();
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                        if let Some(debug_image) = RgbaFrame::from_raw(width, height, &rgba) {
                            let _ = debug_image.save(&p);
                        }
                    } else {
                        let _ = image.save(&p);
                    }
                }
            }
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
        let constant_mb = (target_width == source_width && target_height == source_height)
            .then(|| {
                constant_mb_map(
                    dirty_rects,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                )
            })
            .flatten();
        let encoded =
            self.encoder
                .encode_format_with_constant_mb(&image, *format, constant_mb.as_deref())?;
        let encoded_at = Instant::now();
        if let Some(probe) = self.psnr.as_mut() {
            probe.observe(&image, *format, &encoded.bytes);
        }
        #[cfg(target_os = "linux")]
        trace::encoded(frame, &encoded.bytes, trace_started, trace::now());
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
                queue: preprocess_started.saturating_duration_since(*captured_at),
            },
        ))
    }
}

impl CaptureEncoder {
    fn color_convert_ns(&self) -> u64 {
        self.encoder.color_convert_ns()
    }
}

impl EncoderBackend {
    fn adapt_state(&self) -> Option<(u8, u32)> {
        self.active
            .as_ref()
            .and_then(|active| active.encoder.adapt_state())
    }

    fn color_convert_ns(&self) -> u64 {
        self.active
            .as_ref()
            .map_or(0, |active| active.encoder.color_convert_ns())
    }

    fn new(
        preference: EncoderPreference,
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        probe_dimensions: (u32, u32),
    ) -> Result<Self> {
        Self::new_for_codec(
            VideoCodec::H264,
            false,
            preference,
            bitrate_kbps,
            max_fps,
            latency_mode,
            probe_dimensions,
        )
    }

    fn new_for_codec(
        codec: VideoCodec,
        h264_high: bool,
        preference: EncoderPreference,
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        probe_dimensions: (u32, u32),
    ) -> Result<Self> {
        Self::with_hardware(
            preference,
            EncoderSettings {
                codec,
                h264_high,
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
            remaining_hardware: if matches!(preference, EncoderPreference::Software)
                && settings.codec == VideoCodec::H264
            {
                &[]
            } else {
                hardware
            },
            preference,
            settings,
            dimensions,
            network_constrained: false,
        };
        backend.activate_next(None)?;
        Ok(backend)
    }

    fn activate_next(&mut self, mut last_error: Option<anyhow::Error>) -> Result<()> {
        // Some drivers support only one active hardware session.
        self.active.take();
        while let Some((hardware, remaining)) = self.remaining_hardware.split_first() {
            self.remaining_hardware = remaining;
            if !hardware.codecs.contains(&self.settings.codec) {
                continue;
            }
            match (hardware.open)(self.settings, self.dimensions) {
                Ok(mut encoder) => {
                    encoder.set_network_constrained(self.network_constrained);
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
        let codec = self.settings.codec;
        if codec != VideoCodec::H264 {
            // Only native encoders produce HEVC and AV1. Withdraw the codec so the next
            // negotiation falls back to H.264 instead of repeating this failure.
            crate::codec::withdraw_host_codec(codec);
            return Err(last_error
                .unwrap_or_else(|| anyhow!("no native {} encoder was compiled", codec.label())))
            .context(format!(
                "{} requires a hardware encoder but no usable backend remains",
                codec.label()
            ));
        }
        if matches!(self.preference, EncoderPreference::Hardware) {
            return Err(
                last_error.unwrap_or_else(|| anyhow!("no native hardware encoder was compiled"))
            )
            .context("hardware H.264 was required but no usable backend remains");
        }
        if let Some(error) = last_error {
            tracing::debug!(%error, "no usable hardware encoder; using software encoding");
        }
        let mut encoder = Self::software_encoder(
            force_openh264(std::env::var("NANOCTL_SOFTWARE_ENCODER").ok().as_deref())?,
            self.settings,
            self.dimensions,
        )?;
        encoder.set_network_constrained(self.network_constrained);
        self.active = Some(ActiveEncoder {
            hardware: None,
            encoder,
            needs_initial_idr: true,
        });
        Ok(())
    }

    fn force_keyframe(&mut self) {
        if let Some(active) = &mut self.active {
            active.encoder.force_keyframe();
        }
    }

    fn software_encoder(
        force_openh264: bool,
        settings: EncoderSettings,
        dimensions: (u32, u32),
    ) -> Result<Box<dyn FrameEncoder>> {
        #[cfg(feature = "x264")]
        if !force_openh264 {
            match x264_encoder::X264Encoder::new(
                settings.bitrate_kbps,
                settings.max_fps,
                settings.latency_mode,
                dimensions,
            ) {
                Ok(encoder) => {
                    return Ok(Box::new(x264_encoder::ResilientX264::Native(Box::new(
                        encoder,
                    ))));
                }
                Err(error) => tracing::warn!(%error, "x264 unavailable; using OpenH264"),
            }
        }
        #[cfg(not(feature = "x264"))]
        let _ = (force_openh264, dimensions);
        Ok(Box::new(SoftwareEncoder::new(
            settings.bitrate_kbps,
            settings.max_fps,
            settings.latency_mode,
        )?))
    }

    #[cfg(test)]
    fn software_selected(
        force_openh264: bool,
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        dimensions: (u32, u32),
    ) -> Result<Self> {
        let settings = EncoderSettings {
            codec: VideoCodec::H264,
            h264_high: false,
            bitrate_kbps,
            max_fps,
            latency_mode,
        };
        Ok(Self {
            active: Some(ActiveEncoder {
                hardware: None,
                encoder: Self::software_encoder(force_openh264, settings, dimensions)?,
                needs_initial_idr: true,
            }),
            remaining_hardware: &[],
            preference: EncoderPreference::Software,
            settings,
            dimensions,
            network_constrained: false,
        })
    }

    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        let network_constrained = estimate_kbps < ceiling_kbps;
        if self.network_constrained != network_constrained {
            self.network_constrained = network_constrained;
            if let Some(active) = self.active.as_mut() {
                active.encoder.set_network_constrained(network_constrained);
            }
        }
        let Some(target_kbps) = bitrate_target(
            self.settings.bitrate_kbps,
            estimate_kbps,
            250.min(ceiling_kbps),
            ceiling_kbps,
        ) else {
            return Ok(false);
        };
        let active = self.active.as_mut().context("no active video encoder")?;
        self.settings.bitrate_kbps = target_kbps;
        if let Err(error) = active.encoder.set_bitrate(target_kbps) {
            let Some(hardware) = active.hardware else {
                return Err(error);
            };
            tracing::warn!(backend = hardware.name, %error, "hardware bitrate change failed; switching encoder backend");
            self.activate_next(Some(error.context(hardware.name)))?;
        }
        Ok(true)
    }

    #[cfg(test)]
    fn encode(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
    ) -> Result<EncodedFrame> {
        self.encode_format(image, PixelFormat::Rgba)
    }

    fn encode_format(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
        format: PixelFormat,
    ) -> Result<EncodedFrame> {
        self.encode_format_with_constant_mb(image, format, None)
    }

    fn encode_format_with_constant_mb(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
        format: PixelFormat,
        constant_mb: Option<&[u8]>,
    ) -> Result<EncodedFrame> {
        let view = xcap::image::ImageBuffer::from_raw(
            image.width(),
            image.height(),
            image.as_raw().as_ref(),
        )
        .context("invalid RGBA frame")?;
        let image = &view;
        if self.dimensions != image.dimensions()
            && let Some(active) = &mut self.active
        {
            active.needs_initial_idr = true;
        }
        self.dimensions = image.dimensions();
        loop {
            let active = self.active.as_mut().context("no active video encoder")?;
            let encoded = active
                .encoder
                .encode_format_with_constant_mb(image, format, constant_mb)
                .and_then(|frame| {
                    if active.needs_initial_idr {
                        crate::codec::validate_initial_access_unit(
                            self.settings.codec,
                            &frame.bytes,
                        )?;
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
                    tracing::warn!(backend = hardware.name, %error, "hardware encode failed; switching encoder backend");
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
            .map_or_else(|| self.software_backend().0, |hardware| hardware.name)
    }

    fn id(&self) -> &'static str {
        self.selected_hardware()
            .map_or_else(|| self.software_backend().1, |hardware| hardware.id)
    }

    fn software_backend(&self) -> (&'static str, &'static str) {
        self.active
            .as_ref()
            .map_or(("OpenH264", "software-openh264"), |active| {
                active.encoder.software_backend()
            })
    }
}

fn force_openh264(value: Option<&str>) -> Result<bool> {
    match value {
        None | Some("auto") => Ok(false),
        Some("openh264") => Ok(true),
        Some(_) => bail!("NANOCTL_SOFTWARE_ENCODER must be auto or openh264"),
    }
}

#[cfg(test)]
mod software_selection_tests {
    use super::*;

    #[test]
    fn runtime_override_accepts_auto_or_openh264_and_rejects_empty_or_unknown() {
        assert!(!force_openh264(None).unwrap());
        assert!(!force_openh264(Some("auto")).unwrap());
        assert!(force_openh264(Some("openh264")).unwrap());
        for value in ["", "x264", "OpenH264", "other"] {
            assert!(force_openh264(Some(value)).is_err());
        }
    }

    #[test]
    fn explicit_openh264_override_always_selects_openh264() {
        let backend =
            EncoderBackend::software_selected(true, 4_000, 60, LatencyMode::Balanced, (64, 64))
                .unwrap();
        assert_eq!(backend.name(), "OpenH264");
    }

    #[test]
    fn preferred_software_matches_the_feature_and_init_failure_falls_back() {
        let backend =
            EncoderBackend::software_selected(false, 4_000, 60, LatencyMode::Balanced, (64, 64))
                .unwrap();
        assert_eq!(
            backend.name(),
            if cfg!(feature = "x264") {
                "x264"
            } else {
                "OpenH264"
            }
        );
        let mut backend =
            EncoderBackend::software_selected(false, 4_000, 60, LatencyMode::Balanced, (0, 0))
                .unwrap();
        assert_eq!(backend.name(), "OpenH264");
        let image =
            xcap::image::RgbaImage::from_pixel(64, 64, xcap::image::Rgba([64, 128, 192, 255]));
        assert!(
            !backend
                .encode_format(&image, PixelFormat::Rgba)
                .unwrap()
                .bytes
                .is_empty()
        );
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
}

impl FrameEncoder for SoftwareEncoder {
    fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }

    fn color_convert_ns(&self) -> u64 {
        self.color_convert_ns
    }

    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()> {
        if self.dimensions.is_some() {
            let mut bitrate = SBitrateInfo {
                iLayer: SPATIAL_LAYER_0,
                iBitrate: target_kbps
                    .max(self.bitrate_kbps)
                    .saturating_mul(1_000)
                    .min(i32::MAX as u32) as i32,
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
        Ok(())
    }

    fn encode(&mut self, image: &RgbaFrame<'_>) -> Result<EncodedFrame> {
        self.encode_format(image, PixelFormat::Rgba)
    }

    fn encode_format(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
    ) -> Result<EncodedFrame> {
        self.encode_format_with_constant_mb(image, format, None)
    }

    fn encode_format_with_constant_mb(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
        constant_mb: Option<&[u8]>,
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
        if let Some(constant_mb) = constant_mb {
            yuv.read_pixels_constant_mb(image.as_raw(), format, constant_mb)?;
        } else {
            yuv.read_pixels(image.as_raw(), format)?;
        }
        let color_elapsed = color_started.elapsed();
        self.color_convert_ns += color_elapsed.as_nanos() as u64;
        let bytes = self
            .encoder
            .encode(yuv)
            .context("H.264 encode failed")?
            .to_vec();
        if tracing::enabled!(target: "nanoctl::encoder", tracing::Level::DEBUG)
            && let Ok(statistics) = encoder_statistics(&mut self.encoder)
        {
            tracing::debug!(target: "nanoctl::encoder",
                frame = statistics.uiInputFrameCount,
                qp = statistics.uiAverageFrameQP,
                target_kbps = self.bitrate_kbps,
                bytes = bytes.len(),
                skipped = statistics.uiSkippedFrameCount,
                idr = statistics.uiIDRSentNum,
                convert_us = color_elapsed.as_micros(),
                native_us = color_started.elapsed().saturating_sub(color_elapsed).as_micros(),
                "software encoder frame"
            );
        }
        Ok(EncodedFrame {
            bytes,
            width: image.width(),
            height: image.height(),
        })
    }
}

fn encoder_statistics(encoder: &mut Encoder) -> Result<SEncoderStatistics> {
    let mut statistics = SEncoderStatistics::default();
    // SAFETY: The initialized encoder writes the matching statistics structure synchronously.
    let result = unsafe {
        encoder.raw_api().get_option(
            ENCODER_OPTION_GET_STATISTICS,
            std::ptr::from_mut(&mut statistics).cast(),
        )
    };
    if result != 0 {
        bail!("OpenH264 statistics query failed: {result}");
    }
    Ok(statistics)
}

fn rgba_view(
    frame: &xcap::Frame,
) -> Result<xcap::image::ImageBuffer<xcap::image::Rgba<u8>, &[u8]>> {
    xcap::image::ImageBuffer::from_raw(frame.width, frame.height, frame.raw.as_slice())
        .context("display capture returned an invalid RGBA frame")
}

type StartedCapture = (
    Option<CaptureRecorder>,
    Arc<LatestFrame>,
    Option<std::thread::JoinHandle<()>>,
);

fn start_capture(monitor: &Monitor, stats: Arc<PipelineStats>) -> Result<StartedCapture> {
    #[cfg(target_os = "windows")]
    if !std::env::var("NANOCTL_DXGI_CAPTURE").is_ok_and(|v| v == "0" || v == "false") {
        match media_dxgi::start(monitor, stats.clone()) {
            Ok((latest, thread)) => return Ok((None, latest, Some(thread))),
            Err(error) => {
                tracing::debug!(%error, "DXGI duplication unavailable; retaining continuous capture")
            }
        }
    }
    #[cfg(target_os = "linux")]
    match linux_damage::start(monitor, stats.clone()) {
        Ok((latest, thread)) => return Ok((None, latest, Some(thread))),
        Err(error) => tracing::debug!(%error, "XDamage unavailable; retaining continuous capture"),
    }
    #[cfg(target_os = "linux")]
    {
        stats.damage_enabled.store(false, Ordering::Relaxed);
        stats.capture_parked.store(false, Ordering::Relaxed);
    }
    #[cfg(target_os = "macos")]
    if macos_capture::ScreenRecorder::supported() {
        let latest_frame = Arc::new(LatestFrame {
            stats: stats.clone(),
            ..Default::default()
        });
        match macos_capture::ScreenRecorder::start(monitor.id()?, &latest_frame) {
            Ok(recorder) => {
                tracing::debug!("using ScreenCaptureKit for display capture");
                return Ok((
                    Some(CaptureRecorder::ScreenCaptureKit(recorder)),
                    latest_frame,
                    None,
                ));
            }
            Err(error) => {
                tracing::warn!(%error, "ScreenCaptureKit unavailable; retaining continuous capture")
            }
        }
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
            #[cfg(windows)]
            let _mmcss = register_mmcss();
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
    Ok((
        Some(CaptureRecorder::Xcap(recorder)),
        latest_frame,
        Some(forwarder),
    ))
}

// Slice-parallel OpenH264: encode latency dominates the software path, and
// frame latency matters more than the last few percent of compression here.
fn encode_threads() -> u16 {
    std::env::var("NANOCTL_ENCODE_THREADS")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|n| (1..=16).contains(n))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().min(4) as u16)
                .unwrap_or(2)
        })
}

/// Picks a slice cap that still engages OpenH264's slice-parallel encoder but
/// avoids turning large keyframes into dozens/hundreds of tiny NAL units.
/// Apple decoder stacks handle ordinary multi-slice frames, but very high
/// slice counts are rejected; targeting roughly one slice per encode thread
/// for an average frame keeps both sides inside their sweet spot.
fn openh264_slice_len(bitrate_kbps: u32, max_fps: u16, threads: u16) -> u32 {
    let frame_bytes = u64::from(bitrate_kbps) * 125 / u64::from(max_fps.max(1));
    (frame_bytes / u64::from(threads.max(1))).clamp(2_048, 16_384) as u32
}

fn create_encoder(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Encoder> {
    let (complexity, skip_frames, qp_min) = match latency_mode {
        // Responsiveness caps QP at 26: below ~47dB PSNR the bits are invisible on easy
        // scenes anyway, and the floor only binds there — a ~40% bitstream cut on
        // low-motion desktops at no perceptible quality cost (measured in NOTES.md).
        LatencyMode::Responsiveness => (Complexity::Low, true, 26),
        LatencyMode::Balanced => (Complexity::Medium, true, 14),
        LatencyMode::Quality => (Complexity::High, false, 14),
    };
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate_kbps.saturating_mul(1_000)))
        .max_frame_rate(FrameRate::from_hz(max_fps as f32))
        // Unrepairable IDRs already force a keyframe through the RTCP loss
        // path, so periodic IDRs are pure overhead.
        .intra_frame_period(IntraFramePeriod::from_num_frames(u32::MAX))
        .usage_type(UsageType::CameraVideoRealTime)
        .vui(
            VuiConfig::srgb()
                .full_range(false)
                .matrix_coefficients(MatrixCoefficients::Smpte170M),
        )
        .rate_control_mode(RateControlMode::Bitrate)
        // A zero minimum is OpenH264's sentinel for its lossy 26–35 screen default.
        .qp(QpRange::new(qp_min, 51))
        .complexity(complexity)
        .skip_frames(skip_frames)
        .scene_change_detect(false)
        // Slice-parallel encode: OpenH264 only uses encoder threads when the
        // frame is cut into slices. Scale the cap with the frame-bitrate budget
        // rather than using MTU-sized slices; a fixed tiny cap turns large
        // keyframes into hundreds of slices, which VideoToolbox rejects.
        .max_slice_len(openh264_slice_len(bitrate_kbps, max_fps, encode_threads()))
        .num_threads(encode_threads());
    Encoder::with_api_config(OpenH264API::from_source(), config)
        .context("H.264 encoder is unavailable")
}

pub fn probe_encoder(preference: EncoderPreference) -> Result<(&'static str, &'static str)> {
    let encoder = EncoderBackend::new(preference, 1_000, 30, LatencyMode::Balanced, (1_280, 720))?;
    Ok((encoder.id(), encoder.name()))
}

/// Probes advanced codecs once per process; later calls reuse the recorded host codec set.
pub fn ensure_codecs_probed(preference: EncoderPreference) {
    static PROBED: std::sync::Once = std::sync::Once::new();
    PROBED.call_once(|| {
        probe_codecs(preference);
    });
}

/// Opens a short-lived native encoder for each advanced codec and records which ones this
/// host can produce. H.264 is always included because a software fallback backs it.
pub fn probe_codecs(preference: EncoderPreference) -> Vec<VideoCodec> {
    let mut codecs = Vec::new();
    if !matches!(preference, EncoderPreference::Software) {
        for codec in [VideoCodec::Av1, VideoCodec::Hevc] {
            match EncoderBackend::new_for_codec(
                codec,
                false,
                EncoderPreference::Hardware,
                1_000,
                30,
                LatencyMode::Balanced,
                (1_280, 720),
            ) {
                Ok(encoder) => {
                    tracing::info!(
                        codec = codec.id(),
                        backend = encoder.id(),
                        "hardware encoder available"
                    );
                    codecs.push(codec);
                }
                Err(error) => {
                    tracing::debug!(codec = codec.id(), %error, "hardware encoder unavailable")
                }
            }
        }
    }
    codecs.push(VideoCodec::H264);
    crate::codec::set_host_codecs(&codecs);
    codecs
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
            recorder.stop();
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
    codec: VideoCodec,
    frame: Mutex<Option<EncodedFrame>>,
    ready: tokio::sync::Notify,
    closed: AtomicBool,
    dropped: AtomicU16,
}

impl EncodedFrameSlot {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_codec(VideoCodec::H264)
    }

    fn with_codec(codec: VideoCodec) -> Self {
        Self {
            codec,
            frame: Mutex::new(None),
            ready: tokio::sync::Notify::new(),
            closed: AtomicBool::new(false),
            dropped: AtomicU16::new(0),
        }
    }

    fn is_occupied(&self) -> bool {
        self.frame.lock().is_ok_and(|slot| slot.is_some())
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
        let incoming_is_keyframe = is_keyframe(self.codec, &frame.bytes);
        if slot
            .as_ref()
            .is_some_and(|current| is_keyframe(self.codec, &current.bytes) && !incoming_is_keyframe)
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

struct EncodedFrameProducer(Arc<EncodedFrameSlot>);

impl Drop for EncodedFrameProducer {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[derive(Default)]
struct ReferenceRecovery {
    codec: VideoCodec,
    pending: bool,
}

impl ReferenceRecovery {
    fn admit(
        &mut self,
        bytes: &[u8],
        dropped: u16,
        requests: &tokio::sync::watch::Sender<u64>,
    ) -> bool {
        if dropped > 0 {
            self.pending = true;
            requests.send_modify(|generation| *generation = generation.wrapping_add(1));
            tracing::warn!(dropped, "encoded reference frame discarded; requesting IDR");
        }
        if !self.pending {
            return true;
        }
        if !is_keyframe(self.codec, bytes) {
            return false;
        }
        // A drop beside an already queued IDR can affect frames encoded after that IDR.
        self.pending = dropped > 0;
        true
    }
}

struct TimingSample {
    capture_to_encode: Duration,
    encode: Duration,
    queue: Duration,
}

#[derive(Default)]
struct TimingSamples {
    capture_to_encode_micros: Vec<u64>,
    encode_micros: Vec<u64>,
    queue_micros: Vec<u64>,
}

const MAX_TIMING_SAMPLES: usize = 524_288;

impl TimingSamples {
    fn observe(&mut self, sample: TimingSample, fresh: bool) {
        if fresh && self.capture_to_encode_micros.len() < MAX_TIMING_SAMPLES {
            self.capture_to_encode_micros.push(
                sample
                    .capture_to_encode
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            );
        }
        if self.encode_micros.len() < MAX_TIMING_SAMPLES {
            self.encode_micros
                .push(sample.encode.as_micros().min(u128::from(u64::MAX)) as u64);
        }
        if fresh && self.queue_micros.len() < MAX_TIMING_SAMPLES {
            self.queue_micros
                .push(sample.queue.as_micros().min(u128::from(u64::MAX)) as u64);
        }
    }

    fn percentiles(&mut self) -> TimingPercentiles {
        self.capture_to_encode_micros.sort_unstable();
        self.encode_micros.sort_unstable();
        self.queue_micros.sort_unstable();
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
            queue_p50_milliseconds: percentile_milliseconds(&self.queue_micros, 0.50),
        }
    }
}

struct TimingPercentiles {
    capture_to_encode_p50_milliseconds: f64,
    capture_to_encode_p95_milliseconds: f64,
    encode_p50_milliseconds: f64,
    encode_p95_milliseconds: f64,
    queue_p50_milliseconds: f64,
}

fn percentile_f64(samples: &[f64], percentile: f64) -> f64 {
    let Some(last) = samples.len().checked_sub(1) else {
        return 0.0;
    };
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((last as f64) * percentile).round() as usize;
    sorted[index.min(last)]
}

fn percentile_milliseconds(samples: &[u64], percentile: f64) -> f64 {
    let Some(last) = samples.len().checked_sub(1) else {
        return 0.0;
    };
    let index = ((last as f64) * percentile).round() as usize;
    samples[index.min(last)] as f64 / 1_000.0
}

/// Per-second smoke telemetry so transient stalls, adaptation swings and
/// slow-start effects are visible in JSON instead of hiding in aggregates.
#[derive(Debug, serde::Serialize)]
pub struct SmokeSecond {
    pub second: u64,
    pub frames: u64,
    pub bytes: u64,
    pub encode_sum_milliseconds: f64,
    pub encode_max_milliseconds: f64,
    /// Adaptive x264 tier at the end of this second (None for other backends).
    pub x264_tier: Option<u8>,
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
    pub queue_p50_milliseconds: f64,
    /// Y+U+V PSNR (dB) of each decoded access unit vs its I420 source; None when the
    /// probe was off. p05 is the worst tail — the number a marketing claim must survive.
    pub psnr_p50_db: Option<f64>,
    pub psnr_p05_db: Option<f64>,
    pub psnr_decoded_frames: u64,
    /// Pulls that delivered a newly captured frame (vs a cached one) —
    /// detects capture-pipeline stalls that flat AU sizes alone hide.
    pub fresh_frames: u64,
    /// Access-unit size percentiles (bytes) — exposes stale/delta collapse
    /// that the bitrate average hides.
    pub au_p50_bytes: u64,
    pub au_max_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub idr_frames: u64,
    pub sps_units: u64,
    pub pps_units: u64,
    /// Per-second buckets: frames, bytes, and encode mean/max for that second.
    /// Ordered by `second`; trailing partial second included.
    pub per_second: Vec<SmokeSecond>,
    /// Mean capture-backend CPU copy time per emitted frame.
    pub capture_copy_mean_milliseconds: f64,
    /// Mean RGBA→I420 conversion time per encoded frame (inside encode time).
    pub color_convert_mean_milliseconds: f64,
    /// Final adaptive-negotiation tier and cumulative tier switches (x264 only).
    pub x264_adapt_tier: Option<u8>,
    pub x264_adapt_switches: u32,
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
        if let (Some(p50), Some(p05)) = (self.psnr_p50_db, self.psnr_p05_db) {
            println!(
                "media-smoke psnr_p50_db={:.2} psnr_p05_db={:.2} psnr_frames={}",
                p50, p05, self.psnr_decoded_frames
            );
        }
    }
}

/// `quality.max_fps = 0` follows the primary display's refresh rate, so 120, 144 and 240 Hz
/// monitors stream at their native rate; 60 when the platform does not report one.
pub fn effective_max_fps(configured: u16) -> u16 {
    if configured != 0 {
        return configured;
    }
    Monitor::all()
        .ok()
        .and_then(|monitors| {
            let monitor = monitors
                .iter()
                .find(|monitor| monitor.is_primary().unwrap_or(false))
                .or_else(|| monitors.first())?;
            monitor.frequency().ok()
        })
        .and_then(refresh_to_fps)
        .unwrap_or(60)
}

fn refresh_to_fps(hz: f32) -> Option<u16> {
    // Display-following tops out at 120: no client decodes faster, and pacing
    // an encoder at 240fps on a 240Hz monitor quadrupled encode cost for zero
    // benefit — on a constrained link the overload alone collapsed the stream.
    (hz.is_finite() && hz >= 23.5).then(|| (hz.round() as u16).clamp(24, 120))
}

pub fn run_smoke(
    quality: &QualityConfig,
    seconds: u64,
    measure_psnr: bool,
) -> Result<MediaSmokeReport> {
    let quality = &QualityConfig {
        max_fps: effective_max_fps(quality.max_fps),
        ..quality.clone()
    };
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
        VideoCodec::H264,
        false,
        Arc::default(),
    )?;
    if measure_psnr {
        encoder.enable_psnr_probe()?;
    }
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
    let mut fresh_frames = 0_u64;
    let mut au_sizes: Vec<u64> = Vec::new();
    let mut timing_samples = TimingSamples::default();
    let mut per_second: Vec<SmokeSecond> = Vec::new();
    let mut sec_index = started.elapsed().as_secs();
    let mut sec_frames = 0_u64;
    let mut sec_bytes = 0_u64;
    let mut sec_encode_micros = 0_u128;
    let mut sec_encode_max_micros = 0_u128;
    while Instant::now() < deadline {
        let now = Instant::now();
        if now < next_frame {
            std::thread::sleep(next_frame - now);
        }
        next_frame = Instant::now() + frame_interval;
        let (frame, timing, fresh) =
            encoder.next_access_unit_with_timings(quality.max_width, quality.max_height)?;
        if fresh {
            fresh_frames += 1;
        }
        sec_encode_micros += timing.encode.as_micros();
        sec_encode_max_micros = sec_encode_max_micros.max(timing.encode.as_micros());
        timing_samples.observe(timing, fresh);
        let second = started.elapsed().as_secs();
        if second != sec_index {
            per_second.push(SmokeSecond {
                second: sec_index,
                frames: sec_frames,
                bytes: sec_bytes,
                encode_sum_milliseconds: sec_encode_micros as f64 / 1_000.0,
                encode_max_milliseconds: sec_encode_max_micros as f64 / 1_000.0,
                x264_tier: encoder.adapt_state().map(|(tier, _)| tier),
            });
            sec_index = second;
            sec_frames = 0;
            sec_bytes = 0;
            sec_encode_micros = 0;
            sec_encode_max_micros = 0;
        }
        if frame.bytes.is_empty() {
            continue;
        }
        sec_bytes += frame.bytes.len() as u64;
        let nal_types = annex_b_nal_types(&frame.bytes)?;
        if nal_types.contains(&5) {
            idr_frames += 1;
        }
        sps_units += nal_types.iter().filter(|&&kind| kind == 7).count() as u64;
        pps_units += nal_types.iter().filter(|&&kind| kind == 8).count() as u64;
        encoded_bytes = encoded_bytes
            .checked_add(frame.bytes.len() as u64)
            .context("encoded byte counter overflow")?;
        if au_sizes.len() < MAX_TIMING_SAMPLES {
            au_sizes.push(frame.bytes.len() as u64);
        }
        frames += 1;
        sec_frames += 1;
        if dimensions == (0, 0) {
            dimensions = (frame.width, frame.height);
        }
    }
    if sec_frames > 0 || sec_encode_micros > 0 {
        per_second.push(SmokeSecond {
            second: sec_index,
            frames: sec_frames,
            bytes: sec_bytes,
            encode_sum_milliseconds: sec_encode_micros as f64 / 1_000.0,
            encode_max_milliseconds: sec_encode_max_micros as f64 / 1_000.0,
            x264_tier: encoder.adapt_state().map(|(tier, _)| tier),
        });
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
    let adapt_state = encoder.adapt_state();
    let psnr = encoder
        .psnr_report()
        .map(|(decoded, samples)| (decoded, samples.to_vec()));
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
        queue_p50_milliseconds: timing.queue_p50_milliseconds,
        psnr_p50_db: psnr
            .as_ref()
            .and_then(|(d, s)| (*d > 0).then(|| percentile_f64(s, 0.50))),
        psnr_p05_db: psnr
            .as_ref()
            .and_then(|(d, s)| (*d > 0).then(|| percentile_f64(s, 0.05))),
        psnr_decoded_frames: psnr.map_or(0, |(d, _)| d),
        fresh_frames,
        au_p50_bytes: percentile_f64(
            &au_sizes.iter().map(|size| *size as f64).collect::<Vec<_>>(),
            0.50,
        ) as u64,
        au_max_bytes: au_sizes.iter().copied().max().unwrap_or(0),
        width: dimensions.0,
        height: dimensions.1,
        idr_frames,
        sps_units,
        pps_units,
        per_second,
        capture_copy_mean_milliseconds: {
            let captures = encoder
                .latest_frame
                .stats
                .frames_captured
                .load(Ordering::Relaxed);
            if captures > 0 {
                encoder
                    .latest_frame
                    .stats
                    .capture_ns
                    .load(Ordering::Relaxed) as f64
                    / captures as f64
                    / 1_000_000.0
            } else {
                0.0
            }
        },
        color_convert_mean_milliseconds: if frames > 0 {
            encoder.color_convert_ns() as f64 / frames as f64 / 1_000_000.0
        } else {
            0.0
        },
        x264_adapt_tier: adapt_state.map(|(tier, _)| tier),
        x264_adapt_switches: adapt_state.map_or(0, |(_, switches)| switches),
    })
}

pub(crate) fn annex_b_nal_types(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(crate::codec::annex_b_nal_headers(bytes)?
        .into_iter()
        .map(|header| header & 0x1f)
        .collect())
}

#[derive(Clone, Copy)]
pub struct VideoQuality {
    pub max_bitrate_kbps: u32,
    pub max_fps: u16,
    pub max_width: u32,
    pub max_height: u32,
    pub latency_mode: LatencyMode,
    pub encoder_preference: EncoderPreference,
    pub codec: VideoCodec,
    pub h264_high: bool,
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
        if let Err(error) = self.finish().await {
            tracing::warn!(error = %error, "video pipeline stopped with an error");
        }
    }

    pub(crate) async fn finish(self) -> Result<()> {
        let result = self
            .task
            .await
            .context("video pipeline task stopped unexpectedly")
            .and_then(|result| result);
        self.stats.log();
        result
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

pub(crate) async fn release_session_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if let Err(error) = tokio::task::spawn_blocking(|| {
        let started = Instant::now();
        // glibc releases only free pages; live allocations in other sessions remain valid.
        let trimmed = unsafe { libc::malloc_trim(0) };
        tracing::debug!(
            trim_us = started.elapsed().as_micros(),
            trimmed,
            "session allocator trimmed"
        );
    })
    .await
    {
        tracing::warn!(%error, "session allocator cleanup failed");
    }
}

/// Captures a single frame and encodes it as an IDR into the replay window so
/// a viewer's very first attach is served a stream tail immediately instead
/// of waiting for the new pipeline's first keyframe (~100ms cold). Runs on a
/// blocking thread; a no-op once any stream has warmed the window.
#[cfg(feature = "media")]
pub fn prime_replay(
    max_bitrate_kbps: u32,
    max_fps: u16,
    max_width: u32,
    max_height: u32,
    latency_mode: LatencyMode,
    encoder_preference: EncoderPreference,
) -> Result<()> {
    let replay = crate::rtc::video_replay();
    if replay
        .lock()
        .unwrap()
        .snapshot(crate::codec::VideoCodec::H264)
        .is_some()
    {
        return Ok(());
    }
    let monitor = Monitor::all()
        .context("screen capture is unavailable")?
        .into_iter()
        .find(|monitor| monitor.is_primary().unwrap_or(false))
        .context("no display is available")?;
    // A single-shot grab, not a recorder: a second capture session starves
    // the real pipeline's recorder (its first-frame wait timed out at 5s).
    // On Windows this must use DXGI rather than xcap/WGC: one-shot WGC teardown
    // intermittently unloads GraphicsCapture.dll beneath its own async cleanup.
    #[cfg(target_os = "windows")]
    let captured = media_dxgi::capture_once(&monitor)?;
    #[cfg(not(target_os = "windows"))]
    let captured = {
        let image = monitor.capture_image()?;
        let (width, height) = image.dimensions();
        xcap::Frame::new(width, height, image.into_raw())
    };
    #[cfg(target_os = "windows")]
    let pixel_format = PixelFormat::Bgra;
    #[cfg(not(target_os = "windows"))]
    let pixel_format = PixelFormat::Rgba;
    let (source_width, source_height) = (captured.width, captured.height);
    let (target_width, target_height) =
        fit_dimensions(source_width, source_height, max_width, max_height);
    let resized = if target_width == source_width && target_height == source_height {
        None
    } else {
        let source = rgba_view(&captured)?;
        Some(xcap::image::imageops::resize(
            &source,
            target_width,
            target_height,
            xcap::image::imageops::FilterType::Triangle,
        ))
    };
    let (width, height) = resized
        .as_ref()
        .map(|image| image.dimensions())
        .unwrap_or((source_width, source_height));
    if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
        bail!("captured display dimensions must be non-zero and even");
    }
    // The data-channel path only carries H.264 (rtc.rs gates the channels on
    // the negotiated codec), so the warm keyframe is always baseline H.264.
    let mut backend = EncoderBackend::new_for_codec(
        VideoCodec::H264,
        false,
        encoder_preference,
        max_bitrate_kbps,
        max_fps,
        latency_mode,
        (width, height),
    )?;
    let frame = if let Some(resized) = &resized {
        let view = xcap::image::ImageBuffer::from_raw(width, height, resized.as_raw().as_slice())
            .context("resized display capture is invalid")?;
        backend.encode_format(&view, pixel_format)?
    } else {
        backend.encode_format(&rgba_view(&captured)?, pixel_format)?
    };
    replay
        .lock()
        .unwrap()
        .seed(VideoCodec::H264, video_data_messages(0, true, &frame.bytes));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_video(
    track: Arc<TrackLocalStaticSample>,
    video_data_channel: Arc<crate::rtc::DataChannelVideo>,
    keyframe_requests: tokio::sync::watch::Receiver<u64>,
    link_feedback: tokio::sync::watch::Receiver<Option<crate::adaptive::LinkSample>>,
    viewer_limits: tokio::sync::watch::Receiver<crate::adaptive::ViewerLimits>,
    display_selection: tokio::sync::watch::Receiver<String>,
    display_selection_feedback: tokio::sync::watch::Sender<String>,
    connection_state: tokio::sync::watch::Receiver<
        webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState,
    >,
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
        let encoded_slot = Arc::new(EncodedFrameSlot::with_codec(quality.codec));
        let (delivery_recovery, mut delivery_recovery_requests) =
            tokio::sync::watch::channel(0_u64);
        let producer_slot = EncodedFrameProducer(encoded_slot.clone());
        let producer_stopped = worker_stopped.clone();
        let producer_stats = writer_stats.clone();
        let producer_video_channel = video_data_channel.clone();
        let producer = tokio::task::spawn_blocking(move || -> Result<()> {
            #[cfg(windows)]
            let _mmcss = register_mmcss();
            #[cfg(windows)]
            let _display_wake = DisplayWakeGuard::acquire();
            #[cfg(windows)]
            let _priority = StreamingPriorityGuard::acquire();
            #[cfg(windows)]
            let _wlan = WlanStreamingGuard::acquire();
            let producer_slot = producer_slot;
            #[cfg(target_os = "linux")]
            producer_stats.producer_tid.store(
                std::fs::read_link("/proc/thread-self")
                    .ok()
                    .and_then(|path| path.file_name()?.to_str()?.parse().ok())
                    .unwrap_or_default(),
                Ordering::Relaxed,
            );
            (|| -> Result<()> {
                let mut keyframe_requests = keyframe_requests;
                let mut link_feedback = link_feedback;
                let mut viewer_limits = viewer_limits;
                // A control channel can send viewport limits before this producer
                // subscribes. watch::subscribe starts at the current version, so
                // has_changed alone would silently lose that already-current
                // value; initialize from it explicitly before building the encoder.
                let startup_limits = *viewer_limits.borrow_and_update();
                let startup_dimensions =
                    startup_limits.resolve_dimensions(quality.max_width, quality.max_height);
                let (startup_ceiling, startup_fps_limit) =
                    startup_limits.resolve(quality.max_bitrate_kbps, quality.max_fps);
                let mut controller = controller;
                let startup = controller.set_limits(startup_ceiling, startup_fps_limit);
                let mut encoder = CaptureEncoder::primary(
                    startup.bitrate_kbps,
                    startup_fps_limit,
                    startup_dimensions.0,
                    startup_dimensions.1,
                    quality.latency_mode,
                    quality.encoder_preference,
                    quality.codec,
                    quality.h264_high,
                    producer_stats.clone(),
                )?;
                let mut bitrate_ceiling = startup_ceiling;
                let mut display_selection = display_selection;
                let display_selection_feedback = display_selection_feedback;
                let mut fps = startup.fps;
                let mut pacer = DamagePacer::new(fps);
                let mut scale = EncodeScale::new(startup.bitrate_kbps);
                let mut last: Option<Arc<CapturedFrame>> = None;
                let mut last_encoded: Option<Arc<CapturedFrame>> = None;
                // Damage for captures the pacer dropped still has to reach the
                // next encoded frame; x264's constant-macroblock hints must be
                // relative to the last frame the decoder actually saw.
                let mut damage_since_encode: Option<Vec<DirtyRect>> = None;
                let mut forced_keyframe = false;
                let mut last_forced_idr: Option<Instant> = None;
                let mut dc_keyframes_seen = producer_video_channel
                    .keyframe_requests
                    .load(Ordering::Relaxed);
                let mut dc_backpressure_seen = 0_u64;
                // Explicit requests are the low-latency repair path, but a bounded
                // periodic IDR is the last-resort safety net when a request is lost
                // to cooldown, teardown races, or an idle-transition edge case.
                let mut last_idr_at = Instant::now();
                let mut viewer_dimensions = startup_dimensions;
                producer_stats
                    .target_kbps
                    .store(startup.bitrate_kbps, Ordering::Relaxed);
                producer_stats
                    .target_fps
                    .store(startup.fps, Ordering::Relaxed);
                let mut viewport_bitrate_scale = 1.0;
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
                    // Surface pending keyframe/recovery requests before
                    // blocking on the next capture so an attach or recovery
                    // IDR encodes the frame already in hand instead of
                    // waiting out the pacer deadline (up to 50ms).
                    if keyframe_requests.has_changed().unwrap_or(false) {
                        keyframe_requests.borrow_and_update();
                        forced_keyframe = true;
                    }
                    if delivery_recovery_requests.has_changed().unwrap_or(false) {
                        delivery_recovery_requests.borrow_and_update();
                        forced_keyframe = true;
                    }
                    let dc_keyframes = producer_video_channel
                        .keyframe_requests
                        .load(Ordering::Relaxed);
                    if dc_keyframes != dc_keyframes_seen {
                        dc_keyframes_seen = dc_keyframes;
                        forced_keyframe = true;
                    }
                    if last_idr_at.elapsed() >= PERIODIC_IDR_INTERVAL {
                        forced_keyframe = true;
                    }
                    let now = Instant::now();
                    // The 16ms cap bounds how long a keyframe request stays
                    // unseen while the producer is blocked here: the channel
                    // is only polled after `take` returns. Idle wakeups are
                    // a few atomic ops, so the tighter cap is cheap.
                    let keyframe_ready = forced_keyframe
                        && last_forced_idr.is_none_or(|at| {
                            now.saturating_duration_since(at) >= FORCED_IDR_COOLDOWN
                        });
                    let wait = if producer_slot.0.is_occupied() {
                        // A full handoff means the writer has not consumed the
                        // previous encoded frame. Poll it lightly instead of
                        // running the normal zero-deadline capture loop hot.
                        Duration::from_millis(1)
                    } else if keyframe_ready && last.is_some() {
                        Duration::ZERO
                    } else if last.is_some() {
                        pacer.next_deadline(now, fps).saturating_duration_since(now)
                    } else {
                        Duration::from_millis(50)
                    }
                    .min(Duration::from_millis(16));
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
                    let dc_keyframes = producer_video_channel
                        .keyframe_requests
                        .load(Ordering::Relaxed);
                    if dc_keyframes != dc_keyframes_seen {
                        dc_keyframes_seen = dc_keyframes;
                        forced_keyframe = true;
                    }
                    if last_idr_at.elapsed() >= PERIODIC_IDR_INTERVAL {
                        forced_keyframe = true;
                    }
                    let now = Instant::now();
                    let send_kbps = producer_stats.send_window.lock().unwrap().kbps(now);
                    producer_stats
                        .send_kbps
                        .store(send_kbps as u32, Ordering::Relaxed);
                    controller.update_sender(send_kbps, pacer.idle(), now);
                    let dc_backpressure = producer_stats
                        .dc_backpressure_events
                        .load(Ordering::Relaxed);
                    if dc_backpressure != dc_backpressure_seen {
                        dc_backpressure_seen = dc_backpressure;
                        if let Some(decision) = controller.note_local_backpressure(now) {
                            let (target_kbps, target_ceiling) = viewport_scaled_rate(
                                decision.bitrate_kbps,
                                bitrate_ceiling,
                                viewport_bitrate_scale,
                            );
                            encoder.apply_bitrate_estimate(target_kbps, target_ceiling)?;
                            fps = decision.fps;
                            pacer.set_fps(fps);
                            forced_keyframe |= scale.changed_limits(
                                target_kbps,
                                quality.max_width,
                                quality.max_height,
                            );
                            producer_stats
                                .target_kbps
                                .store(target_kbps, Ordering::Relaxed);
                            producer_stats.target_fps.store(fps, Ordering::Relaxed);
                        }
                    }
                    if link_feedback.has_changed().unwrap_or(false)
                        && let Some(mut sample) = *link_feedback.borrow_and_update()
                    {
                        // While the data channel carries video, RTP only
                        // sends sparse IDRs — the browser's REMB describes
                        // that thin stream, not real egress, so it must not
                        // bind the encoder rate.
                        if producer_video_channel.channel.lock().unwrap().is_some() {
                            sample.remb_kbps = None;
                        }
                        if let Some(decision) = controller.observe(sample) {
                            let (target_kbps, target_ceiling) = viewport_scaled_rate(
                                decision.bitrate_kbps,
                                bitrate_ceiling,
                                viewport_bitrate_scale,
                            );
                            encoder.apply_bitrate_estimate(target_kbps, target_ceiling)?;
                            fps = decision.fps;
                            pacer.set_fps(fps);
                            forced_keyframe |= scale.changed_limits(
                                target_kbps,
                                quality.max_width,
                                quality.max_height,
                            );
                            producer_stats
                                .target_kbps
                                .store(target_kbps, Ordering::Relaxed);
                            producer_stats.target_fps.store(fps, Ordering::Relaxed);
                        }
                    }
                    if viewer_limits.has_changed().unwrap_or(false) {
                        let limits = *viewer_limits.borrow_and_update();
                        let dimensions =
                            limits.resolve_dimensions(quality.max_width, quality.max_height);
                        forced_keyframe |= dimensions != viewer_dimensions;
                        viewer_dimensions = dimensions;
                        let (ceiling, max_fps) =
                            limits.resolve(quality.max_bitrate_kbps, quality.max_fps);
                        let decision = controller.set_limits(ceiling, max_fps);
                        bitrate_ceiling = ceiling;
                        let (target_kbps, target_ceiling) = viewport_scaled_rate(
                            decision.bitrate_kbps,
                            bitrate_ceiling,
                            viewport_bitrate_scale,
                        );
                        encoder.apply_bitrate_estimate(target_kbps, target_ceiling)?;
                        fps = decision.fps;
                        pacer.set_fps(fps);
                        forced_keyframe |= scale.changed_limits(
                            target_kbps,
                            quality.max_width,
                            quality.max_height,
                        );
                        producer_stats
                            .target_kbps
                            .store(target_kbps, Ordering::Relaxed);
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
                                    damage_since_encode = None;
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
                        if let Some(dirty) = fresh.dirty_rects.as_deref() {
                            let damage = damage_since_encode.get_or_insert_with(Vec::new);
                            damage.extend(dirty.iter().copied());
                            compact_dirty_rects(damage);
                        } else {
                            damage_since_encode = None;
                        }
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
                    if producer_slot.0.is_occupied() {
                        // Drop before encoding, not after. Overwriting an
                        // unsent delta would advance the encoder reference
                        // chain without delivering that reference to the
                        // viewer, forcing an otherwise avoidable IDR.
                        producer_stats
                            .backpressure_skips
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    // Forced IDRs must be rate-limited: each one costs
                    // 50-200KB on the wire, so honoring every request or
                    // recovery notice turns congestion into a keyframe storm
                    // that deepens the congestion. A request that arrives during
                    // the cooldown remains pending instead of being discarded;
                    // the producer wakes at least every 16ms and performs it as
                    // soon as the cooldown expires.
                    let now = Instant::now();
                    let force_now = forced_keyframe
                        && last_forced_idr.is_none_or(|at| {
                            now.saturating_duration_since(at) >= FORCED_IDR_COOLDOWN
                        });
                    if force_now {
                        encoder.encoder.force_keyframe();
                        last_forced_idr = Some(now);
                        forced_keyframe = false;
                    }
                    let keepalive = pacer.idle() && !changed && !force_now;
                    #[cfg(target_os = "windows")]
                    let action = pacer.on_capture_frame(Instant::now(), changed, force_now);
                    #[cfg(not(target_os = "windows"))]
                    let action = pacer.on_frame(Instant::now(), changed, force_now);
                    if action == PaceAction::Skip {
                        producer_stats.pacer_skips.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    // While the unordered data channel is the video transport,
                    // idle liveness is a five-byte sequence watermark emitted
                    // by the writer. Encoding the unchanged desktop again only
                    // paid for loss detection, not new image data. RTP fallback
                    // still needs a real frame because it has no such watermark.
                    if keepalive && producer_video_channel.channel.lock().unwrap().is_some() {
                        producer_stats.keepalives.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let (scale_width, scale_height) =
                        scale.limits(quality.max_width, quality.max_height);
                    let max_width = scale_width.min(viewer_dimensions.0);
                    let max_height = scale_height.min(viewer_dimensions.1);
                    let (viewer_fit_width, viewer_fit_height) = fit_dimensions(
                        captured.frame.width,
                        captured.frame.height,
                        viewer_dimensions.0,
                        viewer_dimensions.1,
                    );
                    let viewport_scale = (f64::from(viewer_fit_width)
                        * f64::from(viewer_fit_height)
                        / (f64::from(captured.frame.width) * f64::from(captured.frame.height)))
                    .min(1.0);
                    if (viewport_scale - viewport_bitrate_scale).abs() >= 0.005 {
                        viewport_bitrate_scale = viewport_scale;
                        let decision = controller.decision();
                        let (target_kbps, target_ceiling) = viewport_scaled_rate(
                            decision.bitrate_kbps,
                            bitrate_ceiling,
                            viewport_bitrate_scale,
                        );
                        encoder.apply_bitrate_estimate(target_kbps, target_ceiling)?;
                        producer_stats
                            .target_kbps
                            .store(target_kbps, Ordering::Relaxed);
                    }
                    let (frame, _timing) = encoder.encode_captured_frame_with_damage(
                        captured,
                        max_width,
                        max_height,
                        damage_since_encode.as_deref(),
                    )?;
                    if frame.bytes.is_empty() {
                        continue;
                    }
                    if is_keyframe(quality.codec, &frame.bytes) {
                        last_idr_at = Instant::now();
                    }
                    if let Some(damage) = damage_since_encode.as_mut() {
                        damage.clear();
                    }
                    // `changed` already ran this exact comparison for fresh
                    // frames; only re-compare for keepalive/pace re-encodes.
                    let changed_output = changed
                        || last_encoded
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
                    if !producer_slot.0.replace(frame) {
                        break;
                    }
                }
                Ok(())
            })()
        });
        let mut stop_rx = stop_rx;
        let mut previous_written = None;
        let extensions = [video_playout_delay(quality.latency_mode)];
        let mut stats_tick = tokio::time::interval(Duration::from_secs(
            if tracing::enabled!(
                target: "nanoctl::encoder", tracing::Level::DEBUG
            ) {
                1
            } else {
                10
            },
        ));
        stats_tick.tick().await;
        let mut delivery_interrupted_since: Option<Instant> = None;
        let mut delivery_error_count = 0_u64;
        let mut recovery_keyframe_requested = false;
        let mut dc_backed_up = false;
        let mut dc_seq = 0_u32;
        let mut dc_kf_seen = 0_u64;
        let mut dc_had_channel = false;
        let mut dc_replay: Option<std::collections::VecDeque<Bytes>> = None;
        let mut replay_last_orig: Option<u32> = None;
        let mut replay_last_new = 0_u32;
        let mut dc_was_live = false;
        let mut dc_skips = 0_u64;
        let mut dc_impairment = VideoNetworkImpairment::from_env();
        if dc_impairment.is_some() {
            tracing::info!("test video network impairment enabled");
        }
        let mut reference_recovery = ReferenceRecovery {
            codec: quality.codec,
            ..ReferenceRecovery::default()
        };
        let mut write_result = Ok(());
        let mut connection_state = connection_state;
        let mut transport_recovery_wait = false;
        let mut transport_recovery_requested = false;
        let mut attach_poll = tokio::time::interval(Duration::from_millis(5));
        attach_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat_poll = tokio::time::interval(Duration::from_millis(100));
        heartbeat_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_dc_activity: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = stats_tick.tick() => writer_stats.log(),
                _ = heartbeat_poll.tick() => {
                    let now = Instant::now();
                    if last_dc_activity.is_some_and(|last| {
                        now.saturating_duration_since(last) >= IDLE_KEEPALIVE
                    }) {
                        // Heartbeats do not consume a video sequence number.
                        // Their header carries the next live sequence as a
                        // watermark, letting the viewer detect a lost final
                        // delta without reordering hazards or a redundant AU.
                        last_dc_activity = Some(now);
                        let channel = video_data_channel.channel.lock().unwrap().clone();
                        if let Some(channel) = channel
                            && channel.buffered_amount().await <= 64 * 1024
                        {
                            let msg = video_heartbeat_message(dc_seq);
                            match send_video_data_message(&channel, &msg, false, &mut dc_impairment).await {
                                Ok(true) => {
                                    writer_stats.bytes_sent.fetch_add(msg.len() as u64, Ordering::Relaxed);
                                    writer_stats.send_window.lock().unwrap().writes.push_back((now, msg.len()));
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::warn!(%error, "video data channel heartbeat failed")
                                }
                            }
                        }
                    }
                },
                _ = attach_poll.tick() => {
                    let video_channel = video_data_channel.channel.lock().unwrap().clone();
                    let idr_channel = video_data_channel.idr_channel.lock().unwrap().clone();
                    let has_channel = video_channel.is_some() || idr_channel.is_some();
                    if has_channel && !dc_had_channel {
                        // Show the most recent keyframe immediately, but do not
                        // replay the delta tail: catching up can burst megabytes
                        // into SCTP and make the viewer decode dozens of stale
                        // frames before it reaches the present. The viewer's
                        // attach-time KF produces a fresh IDR instead, while
                        // live deltas are suppressed until it arrives.
                        let replayed = video_data_channel
                            .replay
                            .lock()
                            .unwrap()
                            .snapshot(quality.codec)
                            .map(|idr| {
                                let mut queue = std::collections::VecDeque::new();
                                queue.extend(idr);
                                queue
                            });
                        if replayed.is_some() {
                            tracing::info!("video data channel replaying keyframe to new viewer");
                        }
                        dc_replay = replayed;
                    }
                    dc_had_channel = has_channel;
                    if let Some(queue) = dc_replay.as_mut() {
                        while let Some(msg) = queue.pop_front() {
                            // Replay uses the reliable channel so the keyframe
                            // cannot be overtaken by unordered live traffic.
                            let replay_channel =
                                idr_channel.as_ref().or(video_channel.as_ref());
                            let Some(channel) = replay_channel else {
                                queue.clear();
                                break;
                            };
                            for _ in 0..30 {
                                if channel.buffered_amount().await <= 128 * 1024 {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                            // Renumber into the live sequence and restamp the
                            // send time so the warm keyframe flows straight
                            // into the fresh keyframe that follows it. All
                            // parts of a chunked access unit share one sequence
                            // number.
                            let orig_seq = u32::from_le_bytes(msg[0..4].try_into().unwrap());
                            let new_seq = if Some(orig_seq) == replay_last_orig {
                                replay_last_new
                            } else {
                                let allocated = dc_seq;
                                dc_seq = dc_seq.wrapping_add(1);
                                replay_last_orig = Some(orig_seq);
                                replay_last_new = allocated;
                                allocated
                            };
                            // Replay is the rare path, so only here pay for a
                            // mutable copy with a renumbered/restamped header;
                            // steady-state replay buffering shares the live
                            // message allocation instead of copying it.
                            let mut replay_msg = msg.to_vec();
                            replay_msg[0..4].copy_from_slice(&new_seq.to_le_bytes());
                            let replay_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or_default();
                            // The timestamp offset depends on the fragmentation
                            // flag: chunked messages carry part/parts u16s before
                            // it (offset 9), whole ones do not (offset 5). A fixed
                            // offset clobbered the first payload bytes — the
                            // Annex-B start code — of every unfragmented replayed
                            // keyframe, which is why first-frame decodes failed
                            // with a garbage head until the next natural keyframe.
                            let ts_offset = if replay_msg[4] & 0x02 != 0 { 9 } else { 5 };
                            replay_msg[ts_offset..ts_offset + 8]
                                .copy_from_slice(&replay_ms.to_le_bytes());
                            let replay_msg = Bytes::from(replay_msg);
                            match send_video_data_message(channel, &replay_msg, true, &mut dc_impairment).await {
                                Ok(_) => last_dc_activity = Some(Instant::now()),
                                Err(error) => {
                                    tracing::warn!(%error, "video data channel replay failed");
                                    queue.clear();
                                    break;
                                }
                            }
                        }
                        if queue.is_empty() {
                            dc_replay = None;
                        }
                    }
                }
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                frame = encoded_slot.take() => {
                    let Some((frame, dropped)) = frame else { break };
                    // RTP writes before SRTP is established are silently
                    // discarded. Do not let those discarded frames advance the
                    // encoder reference chain: otherwise the first packets that
                    // do reach the viewer can be undecodable deltas. Discard
                    // frames until the peer is connected, then resume on a
                    // keyframe because the skipped deltas invalidate the chain.
                    let peer_connected = *connection_state.borrow_and_update()
                        == webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Connected;
                    if !peer_connected {
                        if !transport_recovery_requested {
                            transport_recovery_requested = true;
                            delivery_recovery.send_modify(|generation| {
                                *generation = generation.wrapping_add(1);
                            });
                            tracing::info!("video egress waiting for peer connection");
                        }
                        transport_recovery_wait = true;
                        continue;
                    }
                    if transport_recovery_wait {
                        if !crate::codec::is_keyframe(quality.codec, &frame.bytes) {
                            continue;
                        }
                        transport_recovery_wait = false;
                        transport_recovery_requested = false;
                    }
                    if !reference_recovery.admit(&frame.bytes, dropped, &delivery_recovery) {
                        continue;
                    }
                    // A replaced encoded frame is a local latency policy, not RTP packet loss.
                    // Passing that count as prev_dropped_packets corrupts packet sequencing.
                    let written_at = Instant::now();
                    #[cfg(target_os = "linux")]
                    let trace_started = trace::now();
                    let sample = Sample {
                        data: frame.bytes.into(),
                        duration: sample_duration(previous_written, written_at, writer_stats.target_fps.load(Ordering::Relaxed)),
                        ..Default::default()
                    };
                    // A broken transport must not hold capture/encoder teardown hostage forever.
                    // Cancelling the packetizer future is safe; the next successful delivery is
                    // followed by a forced recovery keyframe.
                    // Low-latency path: also ship the same compressed access unit
                    // over the viewer's unreliable data channel when present,
                    // skipping it while its SCTP buffer backs up. Skipped
                    // deltas would wedge the decoder, so a stall requests a
                    // fresh keyframe once throughput recovers.
                    let is_idr = is_keyframe(quality.codec, &sample.data);
                    let dc_kf = video_data_channel.keyframe_requests.load(Ordering::Relaxed);
                    if dc_kf != dc_kf_seen {
                        dc_kf_seen = dc_kf;
                        // The encoded queue is capacity one, so an IDR already
                        // in hand is at most one frame old and satisfies the
                        // request when delivered. Forcing another one here is
                        // what produced back-to-back startup IDRs: the encoder's
                        // natural first keyframe followed immediately by a
                        // forced copy of nearly the same picture.
                        if !is_idr {
                            delivery_recovery.send_modify(|generation| {
                                *generation = generation.wrapping_add(1);
                            });
                        }
                    }
                    let video_channel = video_data_channel.channel.lock().unwrap().clone();
                    let idr_channel = video_data_channel.idr_channel.lock().unwrap().clone();
                    let dc_live = video_channel.is_some();
                    if dc_live != dc_was_live {
                        dc_was_live = dc_live;
                        if !dc_live {
                            // The unreliable video channel just went away and
                            // RTP is the primary path again — the decoder's
                            // reference chain is stale, so force a keyframe
                            // for the resumed track.
                            delivery_recovery.send_modify(|generation| {
                                *generation = generation.wrapping_add(1);
                            });
                        }
                    }
                    // [u32 seq][u8 flags][access unit]; the viewer drops
                    // post-gap deltas and asks for a keyframe on "KF".
                    // Keyframes go over the reliable channel: they are
                    // large and cannot survive partial reliability.
                    let parts = sample
                        .data
                        .len()
                        .div_ceil(if is_idr {
                            VIDEO_DC_KEYFRAME_CHUNK
                        } else {
                            VIDEO_DC_CHUNK
                        })
                        .max(1) as u16;
                    if is_idr {
                        tracing::info!(
                            dc_skips,
                            "video data channel sending IDR seq={dc_seq} parts={parts}"
                        );
                    }
                    let seq = dc_seq;
                    dc_seq = dc_seq.wrapping_add(1);
                    // Messages are built even without an attached viewer so the
                    // replay window stays warm for the next attach.
                    let messages = video_data_messages(seq, is_idr, &sample.data);
                    let target = if is_idr {
                        idr_channel.as_ref().or(video_channel.as_ref())
                    } else {
                        video_channel.as_ref()
                    };
                    let skip_live_delta =
                        video_data_channel.needs_keyframe.load(Ordering::Acquire) && !is_idr;
                    if let Some(channel) = target {
                        if !skip_live_delta {
                            let wire_len = messages.iter().map(|msg| msg.len()).sum::<usize>();
                            // Local backpressure is access-unit atomic. Sending only the
                            // parts that fit before the queue fills wastes constrained
                            // bandwidth on a frame the decoder can never complete.
                            let delta_has_budget = is_idr
                                || channel.buffered_amount().await + wire_len <= 64 * 1024;
                            let mut sent_wire_len = 0_u64;
                            for msg in &messages {
                                if is_idr {
                                    // Pace IDR parts: bursting ~150KB into the
                                    // association starves the congestion window
                                    // and drops deltas on the sibling channel,
                                    // which re-triggers keyframe requests.
                                    for _ in 0..30 {
                                        if channel.buffered_amount().await <= 128 * 1024 {
                                            break;
                                        }
                                        tokio::time::sleep(Duration::from_millis(1)).await;
                                    }
                                    match send_video_data_message(channel, msg, true, &mut dc_impairment).await {
                                        Ok(sent) => {
                                            dc_backed_up = false;
                                            sent_wire_len += u64::from(sent) * msg.len() as u64;
                                        }
                                        Err(error) => {
                                            tracing::warn!(%error, "video data channel send failed");
                                            dc_backed_up = true;
                                            break;
                                        }
                                    }
                                } else if delta_has_budget {
                                    match send_video_data_message(channel, msg, false, &mut dc_impairment).await {
                                        Ok(sent) => {
                                            dc_backed_up = false;
                                            sent_wire_len += u64::from(sent) * msg.len() as u64;
                                        }
                                        Err(error) => {
                                            tracing::warn!(%error, "video data channel send failed");
                                            dc_backed_up = true;
                                            break;
                                        }
                                    }
                                } else {
                                    dc_skips += 1;
                                    if !dc_backed_up {
                                        dc_backed_up = true;
                                        writer_stats
                                            .dc_backpressure_events
                                            .fetch_add(1, Ordering::Relaxed);
                                        delivery_recovery.send_modify(|generation| {
                                            *generation = generation.wrapping_add(1);
                                        });
                                    }
                                    break;
                                }
                            }
                            if is_idr && !dc_backed_up {
                                video_data_channel.needs_keyframe.store(false, Ordering::Release);
                            }
                            if !dc_backed_up {
                                // Count the data-channel egress too: the rate
                                // controller measures real wire bytes, not
                                // just the RTP share.
                                writer_stats.bytes_sent.fetch_add(sent_wire_len, Ordering::Relaxed);
                                writer_stats.send_window.lock().unwrap().writes.push_back((Instant::now(), sent_wire_len as usize));
                                last_dc_activity = Some(Instant::now());
                            }
                        }
                    } else if dc_backed_up {
                        dc_backed_up = false;
                    }
                    // Move the exact wire messages into the replay window after
                    // sending. The transport borrows them while sending, so the
                    // cache no longer clones/copies every access-unit part just
                    // to keep reattach warmup data.
                    {
                        let mut replay = video_data_channel.replay.lock().unwrap();
                        for (part, msg) in messages.into_iter().enumerate() {
                            replay.push(quality.codec, is_idr, part as u16, parts, msg);
                        }
                    }
                    // While the data-channel path is live it carries every AU
                    // and the viewer drops the RTP copy, so RTP video is pure
                    // duplicate bandwidth. That cost is worst exactly during
                    // recovery, when IDRs are largest and the link is already
                    // degraded; duplicating them also makes the rate controller
                    // overcount egress. Channel teardown forces a fresh IDR
                    // before RTP resumes, so no periodic copy is needed for
                    // fallback continuity.
                    if !dc_live {
                        let result = await_video_delivery(
                            async {
                                track
                                    .write_sample_with_extensions(&sample, &extensions)
                                    .await
                                    .map_err(anyhow::Error::from)
                            },
                            VIDEO_SAMPLE_WRITE_TIMEOUT,
                        )
                        .await;
                        writer_stats.send_ns.fetch_add(written_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        writer_stats.sample_writes.fetch_add(1, Ordering::Relaxed);
                        if result.is_ok() {
                            #[cfg(target_os = "linux")]
                            trace::sent(&sample.data, trace_started, trace::now());
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

/// Converts clipped source-space dirty rectangles into x264's row-major
/// 16x16 macroblock hints for the resized encode dimensions. The four-pixel
/// source margin covers scaler/chroma phase bleed; overlap outside a reported
/// dirty region stays marked constant. 1 means provably constant, 0 means
/// x264 must run normal analysis.
fn constant_mb_map(
    dirty_rects: Option<&[DirtyRect]>,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Option<Vec<u8>> {
    let dirty_rects = dirty_rects?;
    if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
        return None;
    }
    let mb_width = target_width.div_ceil(16);
    let mb_height = target_height.div_ceil(16);
    let mut map = vec![1_u8; (mb_width * mb_height) as usize];
    for rect in dirty_rects {
        let left = rect.left.saturating_sub(4).min(source_width) as u64;
        let right = (rect.right as u64 + 4).min(u64::from(source_width));
        let top = rect.top.saturating_sub(4).min(source_height) as u64;
        let bottom = (rect.bottom as u64 + 4).min(u64::from(source_height));
        if right <= left || bottom <= top {
            continue;
        }
        let source_width = u64::from(source_width);
        let source_height = u64::from(source_height);
        let target_width = u64::from(target_width);
        let target_height = u64::from(target_height);
        let x0 = ((left * target_width / source_width) as u32 / 16).min(mb_width);
        let x1 = ((right * target_width).div_ceil(source_width) as u32)
            .div_ceil(16)
            .min(mb_width);
        let y0 = ((top * target_height / source_height) as u32 / 16).min(mb_height);
        let y1 = ((bottom * target_height).div_ceil(source_height) as u32)
            .div_ceil(16)
            .min(mb_height);
        for y in y0..y1 {
            let row = y as usize * mb_width as usize;
            for x in x0..x1 {
                map[row + x as usize] = 0;
            }
        }
    }
    Some(map)
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
        HARDWARE_BACKENDS, HardwareBackend, LatestFrame, PixelFormat, SoftwareEncoder,
        TimingSample, TimingSamples, VIDEO_DC_CHUNK, VIDEO_DC_KEYFRAME_CHUNK,
        VIDEO_DELIVERY_FAILURE_GRACE, annex_b_nal_types, avcc_to_annex_b, await_video_delivery,
        bitrate_target, delivery_failure_grace_elapsed, fit_dimensions, video_data_messages,
        video_heartbeat_message, viewport_scaled_rate,
    };
    use crate::config::{EncoderPreference, LatencyMode};
    use openh264::formats::{RgbSliceU8, YUVBuffer, YUVSource};

    #[test]
    fn video_wire_messages_use_compact_optional_headers() {
        let delta = video_data_messages(1, false, &[7; 10]);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].len(), 15);
        assert_eq!(&delta[0][..5], &[1, 0, 0, 0, 0]);

        let timestamped = video_data_messages(16, false, &[7; 10]);
        assert_eq!(timestamped[0][4], 4);
        assert_eq!(timestamped[0].len(), 23);

        let keyframe = video_data_messages(32, true, &vec![9; 100_000]);
        let expected_parts = 100_000_usize.div_ceil(VIDEO_DC_KEYFRAME_CHUNK);
        assert_eq!(keyframe.len(), expected_parts);
        assert_eq!(keyframe[0][4], 7);
        assert_eq!(
            &keyframe[0][5..9],
            &[0, 0, expected_parts as u8, (expected_parts >> 8) as u8]
        );
        assert_eq!(keyframe[0].len(), 17 + VIDEO_DC_KEYFRAME_CHUNK);
        assert_eq!(
            keyframe[expected_parts - 1].len(),
            17 + (100_000 - (expected_parts - 1) * VIDEO_DC_KEYFRAME_CHUNK)
        );

        let fragmented_delta = video_data_messages(33, false, &vec![8; 10_000]);
        assert_eq!(
            fragmented_delta.len(),
            10_000_usize.div_ceil(VIDEO_DC_CHUNK)
        );
        assert_eq!(fragmented_delta[0].len(), 9 + VIDEO_DC_CHUNK);

        let heartbeat = video_heartbeat_message(0x0102_0304);
        assert_eq!(&heartbeat[..], &[4, 3, 2, 1, 8]);
    }

    #[test]
    fn viewport_bitrate_scaling_preserves_more_bits_per_pixel() {
        assert_eq!(viewport_scaled_rate(12_000, 12_000, 1.0), (12_000, 12_000));
        // 1080p in a 720p viewport has 4/9 of the pixels. The measured 0.8
        // exponent still gives the smaller encode more bits per visible pixel
        // than a linear pixel-ratio cut.
        assert_eq!(
            viewport_scaled_rate(12_000, 12_000, 4.0 / 9.0),
            (6_272, 6_272)
        );
    }
    use std::time::{Duration, Instant};
    use xcap::image::{Rgba, RgbaImage};

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

    #[tokio::test]
    async fn capture_worker_panic_closes_the_encoded_frame_slot() {
        let slot = std::sync::Arc::new(super::EncodedFrameSlot::new());
        let producer = super::EncodedFrameProducer(slot.clone());
        let worker = tokio::task::spawn_blocking(move || {
            let _producer = producer;
            panic!("injected capture worker panic");
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), slot.take())
                .await
                .expect("slot should close after capture worker stops")
                .is_none()
        );
        assert!(worker.await.unwrap_err().is_panic());
    }

    #[tokio::test]
    async fn session_memory_cleanup_preserves_live_buffers_and_is_repeatable() {
        super::release_session_memory().await;
        let pixels = vec![37_u8; 4 * 1024 * 1024];
        super::release_session_memory().await;
        assert!(pixels.iter().all(|byte| *byte == 37));
        drop(pixels);
        super::release_session_memory().await;
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
        assert_eq!(scale.limits(3840, 2160), (960, 540));
        assert_eq!(scale.limits(1280, 720), (320, 180));
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
    fn desktop_playout_is_immediate_except_when_quality_buffering_is_requested() {
        for (mode, max_delay) in [
            (LatencyMode::Responsiveness, 0),
            (LatencyMode::Balanced, 0),
            (LatencyMode::Quality, 10),
        ] {
            let webrtc::rtp::extension::HeaderExtension::PlayoutDelay(delay) =
                super::video_playout_delay(mode)
            else {
                panic!("Expected playout delay")
            };
            assert_eq!(delay.min_delay, 0);
            assert_eq!(delay.max_delay, max_delay);
        }
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
        assert!(!queue.take(Duration::ZERO).unwrap().1);
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
    fn software_quality_floor_still_honors_a_congestion_bitrate_cut() {
        let mut encoder = SoftwareEncoder::new(4_000, 60, LatencyMode::Balanced).unwrap();
        let mut image = xcap::image::RgbaImage::new(256, 128);
        let mut seed = 1_u32;
        let mut before_bytes = 0;
        let mut after_bytes = 0;
        for frame in 0..40 {
            for pixel in image.pixels_mut() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *pixel = xcap::image::Rgba([
                    (seed >> 24) as u8,
                    (seed >> 16) as u8,
                    (seed >> 8) as u8,
                    255,
                ]);
            }
            if frame == 10 {
                encoder.set_bitrate(250).unwrap();
            }
            let view = super::RgbaFrame::from_raw(
                image.width(),
                image.height(),
                image.as_raw().as_slice(),
            )
            .unwrap();
            let encoded = encoder.encode(&view).unwrap();
            if frame < 10 {
                before_bytes += encoded.bytes.len();
            } else if frame >= 20 {
                after_bytes += encoded.bytes.len();
            }
        }
        assert_eq!(encoder.bitrate_kbps, 250);
        assert!(before_bytes > 0);
        assert!(
            after_bytes < before_bytes / 2,
            "congested twenty-frame output {after_bytes}, clean ten-frame output {before_bytes}"
        );
    }

    #[test]
    fn software_bitrate_changes_at_runtime_without_forcing_an_idr() {
        let pixels =
            xcap::image::RgbaImage::from_pixel(64, 64, xcap::image::Rgba([32, 96, 160, 255]));
        let image = xcap::image::ImageBuffer::from_raw(64, 64, pixels.as_raw().as_slice()).unwrap();
        let mut encoder = SoftwareEncoder::new(1_000, 30, LatencyMode::Quality).unwrap();
        encoder.set_bitrate(1_100).unwrap();
        let first = encoder.encode(&image).unwrap();
        assert!(annex_b_nal_types(&first.bytes).unwrap().contains(&5));
        for target in [900, 2_000, 1_200] {
            encoder.set_bitrate(target).unwrap();
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
            assert!(
                types.contains(&1),
                "types={types:?} bytes={}",
                next.bytes.len()
            );
            assert!(!types.contains(&5));
        }
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
            format: PixelFormat::Rgba,
            captured_at: now,
            backend_changed: None,
            emit_gen: 0,
            dirty_rects: None,
            recycle: None,
        };
        let mut next = previous.clone();
        next.captured_at += Duration::from_secs(1);
        assert!(!next.differs_from(&previous));
        next.format = PixelFormat::Bgra;
        assert!(next.differs_from(&previous));
        next.format = PixelFormat::Rgba;
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

    fn reference_rgba<const ROUND: u32>(frame: &mut super::I420Frame, rgba: &[u8]) {
        let pixels = frame.width * frame.height;
        let (y, uv) = frame.bytes.split_at_mut(pixels);
        let (u, v) = uv.split_at_mut(pixels / 4);
        for (pixel, y) in rgba.chunks_exact(4).zip(y) {
            *y = (((66 * u32::from(pixel[0])
                + 129 * u32::from(pixel[1])
                + 25 * u32::from(pixel[2])
                + ROUND)
                >> 8)
                + 16) as u8;
        }
        for ((rows, u), v) in rgba
            .chunks_exact(frame.width * 8)
            .zip(u.chunks_exact_mut(frame.width / 2))
            .zip(v.chunks_exact_mut(frame.width / 2))
        {
            let (top, bottom) = rows.split_at(frame.width * 4);
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
                *u = (((-38 * r + 112 * b - 74 * g + ROUND as i16) >> 8) + 128) as u8;
                *v = (((112 * r - 18 * b - 94 * g + ROUND as i16) >> 8) + 128) as u8;
            }
        }
    }

    #[test]
    #[ignore = "isolated 720p conversion timing; run release, single test thread"]
    fn benchmark_rgba_conversion_720p() {
        use std::hint::black_box;
        let rgba: Vec<_> = (0..1280 * 720 * 4)
            .map(|i| (i * 71 + i / 997) as u8)
            .collect();
        let mut frame = super::I420Frame::new(1280, 720).unwrap();
        let mut samples = [Vec::new(), Vec::new(), Vec::new()];
        for batch in 0..9 {
            for position in 0..3 {
                let variant = (batch + position) % 3;
                let started = Instant::now();
                for _ in 0..50 {
                    let frame = black_box(&mut frame);
                    let rgba = black_box(&rgba);
                    match variant {
                        0 => reference_rgba::<0>(frame, rgba),
                        1 => reference_rgba::<128>(frame, rgba),
                        _ => frame.read_rgba(rgba).unwrap(),
                    }
                    black_box(&frame.bytes);
                }
                samples[variant].push(started.elapsed().as_nanos() / 50);
            }
        }
        for (name, mut samples) in [
            "truncating-reference",
            "rounded-reference",
            "production-rounded",
        ]
        .into_iter()
        .zip(samples)
        {
            samples.sort_unstable();
            println!(
                "{name}: ns/frame median={} min={} max={}",
                samples[4], samples[0], samples[8]
            );
        }
    }

    #[test]
    fn direct_rgba_conversion_rejects_empty_and_short_buffers_without_mutation() {
        let mut frame = super::I420Frame::new(2, 2).unwrap();
        frame.read_rgba(&[255; 16]).unwrap();
        let before = frame.bytes.clone();
        assert!(frame.read_rgba(&[]).is_err());
        assert!(frame.read_rgba(&[0; 15]).is_err());
        assert_eq!(frame.bytes, before);
    }

    #[test]
    fn direct_rgba_conversion_checks_truncated_and_rounded_kernels() {
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
                let mut expected = super::I420Frame::new(width, height).unwrap();
                reference_rgba::<0>(&mut expected, &rgba);
                assert_eq!(expected.y(), reference.y());
                assert_eq!(expected.u(), reference.u());
                assert_eq!(expected.v(), reference.v());
                reference_rgba::<128>(&mut expected, &rgba);
                converted.read_rgba(&rgba).unwrap();
                assert_eq!(converted.bytes.as_ptr(), allocation);
                assert_eq!(converted.strides(), reference.strides());
                // SIMD conversion may round within 1 LSB of the scalar kernel.
                let max_delta = converted
                    .bytes
                    .iter()
                    .zip(expected.bytes.iter())
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0);
                assert!(max_delta <= 1, "max channel delta {max_delta}");
            }
        }
    }

    #[test]
    fn direct_bgra_conversion_matches_swizzled_rgba_input() {
        let mut seed = 0x8765_4321_u32;
        let bgra: Vec<_> = (0..32 * 18 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let mut rgba = bgra.clone();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mut from_bgra = super::I420Frame::new(32, 18).unwrap();
        let mut expected = super::I420Frame::new(32, 18).unwrap();
        from_bgra.read_pixels(&bgra, PixelFormat::Bgra).unwrap();
        expected.read_rgba(&rgba).unwrap();
        let max_delta = from_bgra
            .bytes
            .iter()
            .zip(expected.bytes.iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(max_delta <= 1, "max channel delta {max_delta}");
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
            encoder: EncoderBackend::new(
                EncoderPreference::Software,
                1_000,
                30,
                LatencyMode::Quality,
                (64, 64),
            )
            .unwrap(),
            psnr: None,
            #[cfg(target_os = "windows")]
            resizer: super::resize::RgbaResizer::default(),
        };
        let mut captured = super::CapturedFrame {
            frame: xcap::Frame::new(64, 64, vec![128; 64 * 64 * 4]),
            format: PixelFormat::Rgba,
            captured_at: Instant::now() - Duration::from_secs(10),
            backend_changed: None,
            emit_gen: 0,
            dirty_rects: None,
            recycle: None,
        };
        let (frame, _) = encoder.encode_captured_frame(&captured, 64, 64).unwrap();
        assert!(!frame.bytes.is_empty());
        encoder.encoder.force_keyframe();
        let (forced, _) = encoder.encode_captured_frame(&captured, 64, 64).unwrap();
        assert!(!forced.bytes.is_empty());
        let (resized, _) = encoder.encode_captured_frame(&captured, 32, 32).unwrap();
        assert_eq!((resized.width, resized.height), (32, 32));
        assert!(!resized.bytes.is_empty());
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
    fn constant_macroblock_maps_mark_only_expanded_dirty_regions() {
        let rect = super::DirtyRect {
            left: 20,
            top: 20,
            right: 28,
            bottom: 28,
        };
        let map = super::constant_mb_map(Some(&[rect]), 64, 64, 64, 64).unwrap();
        assert_eq!(map.len(), 16);
        assert_eq!(map[1 + 4], 0);
        assert_eq!(map.iter().filter(|value| **value == 0).count(), 1);

        let scaled = super::constant_mb_map(
            Some(&[super::DirtyRect {
                left: 32,
                top: 32,
                right: 48,
                bottom: 48,
            }]),
            64,
            64,
            32,
            32,
        )
        .unwrap();
        assert_eq!(scaled.len(), 4);
        assert_eq!(scaled, [0, 0, 0, 0]);

        let empty = super::constant_mb_map(Some(&[]), 64, 64, 64, 64).unwrap();
        assert!(empty.iter().all(|value| *value == 1));
        assert!(super::constant_mb_map(None, 64, 64, 64, 64).is_none());
    }

    #[test]
    fn replaced_captures_accumulate_dirty_hints() {
        let latest = super::LatestFrame::default();
        let first = super::DirtyRect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
        let second = super::DirtyRect {
            left: 1,
            top: 1,
            right: 2,
            bottom: 2,
        };
        assert!(latest.replace_hint(
            xcap::Frame::new(2, 2, vec![0; 16]),
            PixelFormat::Rgba,
            Some(true),
            0,
            Some(vec![first]),
        ));
        assert!(latest.replace_hint(
            xcap::Frame::new(2, 2, vec![1; 16]),
            PixelFormat::Rgba,
            Some(true),
            0,
            Some(vec![second]),
        ));
        let (frame, fresh) = latest.take(Duration::from_millis(1)).unwrap();
        assert!(fresh);
        let dirty = frame.dirty_rects.as_deref().unwrap().as_slice();
        assert_eq!(dirty.len(), 2);
        assert!(dirty.contains(&first));
        assert!(dirty.contains(&second));
    }

    #[test]
    fn partial_i420_conversion_is_bit_identical_when_damage_is_covered() {
        let mut before = Vec::with_capacity(64 * 64 * 4);
        for index in 0..64 * 64 {
            before.extend_from_slice(&[
                (index % 251) as u8,
                (index % 241) as u8,
                (index % 229) as u8,
                255,
            ]);
        }
        let mut after = before.clone();
        for y in 20..32 {
            for x in 20..28 {
                let offset = (y * 64 + x) * 4;
                after[offset..offset + 4].copy_from_slice(&[250, 5, 120, 255]);
            }
        }
        // Include damage in a bottom-edge macroblock which does not start at
        // the left edge. That shape needs leftward widening because the `yuv`
        // row iterator requires stride padding after the frame's final row.
        for y in 48..64 {
            for x in 48..64 {
                let offset = (y * 64 + x) * 4;
                after[offset..offset + 4].copy_from_slice(&[20, 240, 90, 255]);
            }
        }
        let mut full_before = super::I420Frame::new(64, 64).unwrap();
        full_before.read_pixels(&before, PixelFormat::Rgba).unwrap();
        let mut full_after = super::I420Frame::new(64, 64).unwrap();
        full_after.read_pixels(&after, PixelFormat::Rgba).unwrap();
        let mut partial = super::I420Frame::new(64, 64).unwrap();
        partial.bytes = full_before.bytes.clone();
        let mut map = vec![1_u8; 16];
        map[1 + 4] = 0;
        map[3 + 3 * 4] = 0;
        partial
            .read_pixels_constant_mb(&after, PixelFormat::Rgba, &map)
            .unwrap();
        assert_eq!(partial.bytes, full_after.bytes);

        let mut idle = super::I420Frame::new(64, 64).unwrap();
        idle.bytes = full_before.bytes.clone();
        idle.read_pixels_constant_mb(&after, PixelFormat::Rgba, &[1_u8; 16])
            .unwrap();
        assert_eq!(idle.bytes, full_before.bytes);
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
    fn local_reference_loss_suppresses_deltas_until_a_clean_idr() {
        let (requests, mut observed) = tokio::sync::watch::channel(0_u64);
        let mut recovery = super::ReferenceRecovery::default();
        let delta = [0, 0, 0, 1, 0x41];
        let idr = [0, 0, 0, 1, 0x65];
        assert!(recovery.admit(&delta, 0, &requests));
        assert!(!observed.has_changed().unwrap());
        assert!(!recovery.admit(&delta, 1, &requests));
        assert!(observed.has_changed().unwrap());
        assert_eq!(*observed.borrow_and_update(), 1);
        assert!(!recovery.admit(&delta, 0, &requests));
        assert!(!observed.has_changed().unwrap());
        assert!(recovery.admit(&idr, 0, &requests));
        assert!(recovery.admit(&delta, 0, &requests));
    }

    #[test]
    fn dropping_a_delta_beside_a_queued_idr_still_requires_a_later_idr() {
        let (requests, _) = tokio::sync::watch::channel(0_u64);
        let mut recovery = super::ReferenceRecovery::default();
        let idr = [0, 0, 1, 0x65];
        let delta = [0, 0, 1, 0x41];
        assert!(recovery.admit(&idr, 1, &requests));
        assert!(!recovery.admit(&delta, 0, &requests));
        assert!(recovery.admit(&idr, 0, &requests));
        assert!(recovery.admit(&delta, 0, &requests));
    }

    #[test]
    fn local_reference_recovery_handles_empty_truncated_and_closed_signals() {
        let (requests, observed) = tokio::sync::watch::channel(u64::MAX);
        drop(observed);
        let mut recovery = super::ReferenceRecovery::default();
        assert!(!recovery.admit(&[], u16::MAX, &requests));
        assert_eq!(*requests.borrow(), 0);
        assert!(!recovery.admit(&[0, 0, 1], 0, &requests));
        assert!(recovery.admit(&[0, 0, 1, 0x65], 0, &requests));
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
        let (frame, fresh) = queue.take(Duration::from_millis(1)).unwrap();
        assert_eq!(frame.frame.raw[0], 2);
        assert!(fresh);
    }

    #[test]
    fn media_smoke_does_not_measure_cached_frame_age_as_capture_latency() {
        let mut timings = TimingSamples::default();
        for (fresh, age) in [
            (true, Duration::from_millis(20)),
            (false, Duration::from_secs(3)),
        ] {
            timings.observe(
                TimingSample {
                    capture_to_encode: age,
                    encode: Duration::from_millis(4),
                    queue: Duration::ZERO,
                },
                fresh,
            );
        }
        assert_eq!(timings.capture_to_encode_micros, [20_000]);
        assert_eq!(timings.encode_micros, [4_000, 4_000]);
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
    fn openh264_slice_len_targets_encode_threads_without_tiny_slices() {
        assert_eq!(super::openh264_slice_len(8_000, 60, 4), 4_166);
        assert_eq!(super::openh264_slice_len(24_000, 60, 4), 12_500);
        assert_eq!(super::openh264_slice_len(100, 60, 4), 2_048);
        assert_eq!(super::openh264_slice_len(900_000, 60, 1), 16_384);
    }

    #[test]
    fn display_refresh_rates_map_to_frame_rate_caps() {
        assert_eq!(super::refresh_to_fps(59.94), Some(60));
        assert_eq!(super::refresh_to_fps(143.9), Some(120));
        assert_eq!(super::refresh_to_fps(360.0), Some(120));
        assert_eq!(super::refresh_to_fps(0.0), None);
        assert_eq!(super::refresh_to_fps(f32::NAN), None);
        assert_eq!(super::effective_max_fps(90), 90);
    }

    const TEST_SETTINGS: EncoderSettings = EncoderSettings {
        codec: crate::codec::VideoCodec::H264,
        h264_high: false,
        bitrate_kbps: 4_000,
        max_fps: 30,
        latency_mode: LatencyMode::Balanced,
    };

    #[test]
    fn adaptive_hardware_receives_bitrate_updates_and_borrowed_pixels() {
        struct AdaptiveHardware {
            bitrate_kbps: u32,
            pixels: usize,
            network_constrained: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }

        impl FrameEncoder for AdaptiveHardware {
            fn force_keyframe(&mut self) {}

            fn set_bitrate(&mut self, target_kbps: u32) -> anyhow::Result<()> {
                self.bitrate_kbps = target_kbps;
                Ok(())
            }

            fn set_network_constrained(&mut self, constrained: bool) {
                self.network_constrained
                    .store(constrained, std::sync::atomic::Ordering::Relaxed);
            }

            fn encode(&mut self, image: &super::RgbaFrame<'_>) -> anyhow::Result<EncodedFrame> {
                assert_eq!(self.pixels, image.as_raw().as_ptr() as usize);
                assert_eq!(self.bitrate_kbps, u32::from(image.get_pixel(0, 0)[0]) * 100);
                Ok(EncodedFrame {
                    bytes: vec![0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1],
                    width: image.width(),
                    height: image.height(),
                })
            }
        }

        let mut image = RgbaImage::from_pixel(64, 64, Rgba([40, 96, 160, 255]));
        let hardware_constrained = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut encoder = EncoderBackend {
            active: Some(super::ActiveEncoder {
                hardware: Some(&FAILING_HARDWARE),
                encoder: Box::new(AdaptiveHardware {
                    bitrate_kbps: TEST_SETTINGS.bitrate_kbps,
                    pixels: image.as_raw().as_ptr() as usize,
                    network_constrained: hardware_constrained.clone(),
                }),
                needs_initial_idr: true,
            }),
            remaining_hardware: &[],
            preference: EncoderPreference::Auto,
            settings: TEST_SETTINGS,
            dimensions: image.dimensions(),
            network_constrained: false,
        };
        encoder.encode(&image).unwrap();
        for (target, constrained) in [(2_000, true), (6_000, true), (8_000, false)] {
            assert!(encoder.apply_bitrate_estimate(target, 8_000).unwrap());
            assert_eq!(encoder.network_constrained, constrained);
            assert_eq!(
                hardware_constrained.load(std::sync::atomic::Ordering::Relaxed),
                constrained
            );
            image.get_pixel_mut(0, 0)[0] = (target / 100) as u8;
            encoder.encode(&image).unwrap();
            assert_eq!(encoder.id(), "first");
        }
    }

    struct FakeHardware {
        frames_left: usize,
        missing_headers: bool,
    }

    impl FrameEncoder for FakeHardware {
        fn force_keyframe(&mut self) {}

        fn set_bitrate(&mut self, _target_kbps: u32) -> anyhow::Result<()> {
            anyhow::bail!("simulated bitrate rejection")
        }

        fn encode(&mut self, image: &super::RgbaFrame<'_>) -> anyhow::Result<EncodedFrame> {
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
        codecs: &[crate::codec::VideoCodec::H264],
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
        codecs: &[crate::codec::VideoCodec::H264],
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
            codecs: &[crate::codec::VideoCodec::H264],
            open: |_, _| anyhow::bail!("simulated missing driver"),
        }];
        let open = |preference| {
            EncoderBackend::with_hardware(preference, TEST_SETTINGS, (64, 64), MISSING)
        };
        assert_eq!(
            open(EncoderPreference::Auto).unwrap().id(),
            preferred_software_id()
        );
        assert!(open(EncoderPreference::Hardware).is_err());
        const NEVER_OPEN: &[HardwareBackend] = &[HardwareBackend {
            id: "never",
            name: "Forbidden native backend",
            codecs: &[crate::codec::VideoCodec::H264],
            open: |_, _| panic!("software mode probed a hardware driver"),
        }];
        let software = EncoderBackend::with_hardware(
            EncoderPreference::Software,
            TEST_SETTINGS,
            (64, 64),
            NEVER_OPEN,
        )
        .unwrap();
        assert_eq!(software.id(), preferred_software_id());
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
        assert_eq!(encoder.id(), preferred_software_id());
        encoder.encode(&image).unwrap();
        assert_eq!(encoder.id(), preferred_software_id());
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
            codecs: &[crate::codec::VideoCodec::H264],
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
        assert_eq!(no_headers.id(), preferred_software_id());
        let mut retune_failed = open(&[FAILING_HARDWARE]);
        retune_failed.encode(&image).unwrap();
        assert!(retune_failed.apply_bitrate_estimate(2_000, 4_000).unwrap());
        assert_eq!(retune_failed.settings.bitrate_kbps, 2_000);
        assert_eq!(retune_failed.id(), preferred_software_id());
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

    fn preferred_software_id() -> &'static str {
        if cfg!(feature = "x264") {
            "software-x264"
        } else {
            "software-openh264"
        }
    }

    #[test]
    #[ignore = "opt-in encode-only performance measurement"]
    fn benchmark_encoder() {
        let requested =
            std::env::var("NANOCTL_BENCH_ENCODER").unwrap_or("software-openh264".into());
        let dimensions = (1_920, 1_080);
        let mut encoder = if requested == "software-openh264" {
            EncoderBackend::software_selected(
                true,
                TEST_SETTINGS.bitrate_kbps,
                TEST_SETTINGS.max_fps,
                TEST_SETTINGS.latency_mode,
                dimensions,
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
    fn software_encoder_reports_the_last_encoded_frame_statistics() {
        let mut encoder = super::create_encoder(4_000, 60, LatencyMode::Balanced).unwrap();
        assert!(super::encoder_statistics(&mut encoder).is_err());
        let frame = super::I420Frame::new(32, 32).unwrap();
        for count in 1..=3 {
            assert!(!encoder.encode(&frame).unwrap().to_vec().is_empty());
            let statistics = super::encoder_statistics(&mut encoder).unwrap();
            assert_eq!(statistics.uiInputFrameCount, count);
            assert_eq!((statistics.uiWidth, statistics.uiHeight), (32, 32));
            assert!(statistics.uiAverageFrameQP <= 51);
        }
    }

    #[test]
    fn software_encoder_does_not_schedule_periodic_idrs() {
        let mut encoder = super::create_encoder(4_000, 30, LatencyMode::Balanced).unwrap();
        let mut frame = super::I420Frame::new(32, 32).unwrap();
        frame.read_rgba(&[128; 32 * 32 * 4]).unwrap();
        let mut idrs = 0;
        for _ in 0..=30 * 30 {
            let bytes = encoder.encode(&frame).unwrap().to_vec();
            if !bytes.is_empty() && super::annex_b_nal_types(&bytes).unwrap().contains(&5) {
                idrs += 1;
            }
        }
        assert_eq!(idrs, 1);
    }

    #[test]
    fn software_encoder_signals_the_limited_bt601_matrix_of_its_srgb_input() {
        for mode in [
            LatencyMode::Responsiveness,
            LatencyMode::Balanced,
            LatencyMode::Quality,
        ] {
            let mut encoder = super::create_encoder(4_000, 60, mode).unwrap();
            let mut frame = super::I420Frame::new(16, 16).unwrap();
            frame.read_rgba(&[128; 16 * 16 * 4]).unwrap();
            encoder.encode(&frame).unwrap();
            let mut params = openh264_sys2::SEncParamExt::default();
            // SAFETY: GetOption synchronously writes the matching initialized parameter structure.
            let result = unsafe {
                encoder.raw_api().get_option(
                    openh264_sys2::ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                    std::ptr::from_mut(&mut params).cast(),
                )
            };
            assert_eq!(result, 0);
            // Responsiveness floors QP at 26 to stop paying for invisible quality on
            // easy scenes; the fidelity modes keep the full range.
            let expected_min_qp = match mode {
                LatencyMode::Responsiveness => 26,
                _ => 14,
            };
            assert_eq!((params.iMinQp, params.iMaxQp), (expected_min_qp, 51));
            assert_eq!(params.iRCMode, openh264_sys2::RC_BITRATE_MODE);
            assert_eq!(params.iUsageType, openh264_sys2::CAMERA_VIDEO_REAL_TIME);
            let layer = &params.sSpatialLayers[0];
            assert!(layer.bVideoSignalTypePresent);
            assert!(layer.bColorDescriptionPresent);
            assert!(!layer.bFullRange);
            assert_eq!(layer.uiColorPrimaries, 1);
            assert_eq!(layer.uiTransferCharacteristics, 13);
            assert_eq!(layer.uiColorMatrix, 6);
        }
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
