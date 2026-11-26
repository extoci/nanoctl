use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use openh264::formats::YUVSource;

#[cfg(test)]
use super::bitrate_target;
use super::{EncodedFrame, FrameEncoder, I420Frame, PixelFormat, RgbaFrame, SoftwareEncoder};
use crate::config::LatencyMode;

unsafe extern "C" {
    fn nano_x264_open(
        width: c_int,
        height: c_int,
        kbps: c_int,
        fps: c_int,
        frames: c_int,
        library: *const c_char,
    ) -> *mut c_void;
    fn nano_x264_close(context: *mut c_void);
    fn nano_x264_bitrate(context: *mut c_void, kbps: c_int, fps: c_int, frames: c_int) -> c_int;
    fn nano_x264_encode(
        context: *mut c_void,
        y: *const u8,
        u: *const u8,
        v: *const u8,
        width: c_int,
        pts: i64,
        keyframe: c_int,
        bytes: *mut *const u8,
    ) -> c_int;
    fn nano_x264_adapt(context: *mut c_void, tier: c_int) -> c_int;
    #[cfg(test)]
    fn nano_x264_parameters(context: *mut c_void, params: *mut Parameters);
}

fn buffer_frames(mode: LatencyMode) -> c_int {
    match mode {
        LatencyMode::Responsiveness => 1,
        LatencyMode::Balanced => 2,
        LatencyMode::Quality => 4,
    }
}

struct Active {
    handle: NonNull<c_void>,
    dimensions: (u32, u32),
}

impl Active {
    fn new(
        dimensions: (u32, u32),
        bitrate_kbps: u32,
        max_fps: u16,
        mode: LatencyMode,
        library: Option<&CStr>,
    ) -> Result<Self> {
        let (width, height) = dimensions;
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            bail!("x264 dimensions must be non-zero and even");
        }
        // SAFETY: Only scalar parameters and an optional NUL-terminated library name enter
        // the shim. A successful open transfers ownership of one opaque encoder to us.
        let handle = unsafe {
            nano_x264_open(
                width.try_into().context("x264 width overflows")?,
                height.try_into().context("x264 height overflows")?,
                bitrate_kbps.try_into().context("x264 bitrate overflows")?,
                max_fps.into(),
                buffer_frames(mode),
                library.map_or(std::ptr::null(), CStr::as_ptr),
            )
        };
        Ok(Self {
            handle: NonNull::new(handle)
                .context("system libx264 is missing, ABI-incompatible, or could not initialize")?,
            dimensions,
        })
    }

    fn encode(&mut self, yuv: &I420Frame, pts: i64, keyframe: bool) -> Result<Vec<u8>> {
        if yuv.dimensions() != (self.dimensions.0 as usize, self.dimensions.1 as usize) {
            bail!("I420 buffer does not match x264 dimensions");
        }
        let mut bytes = std::ptr::null();
        // SAFETY: All three planes match this encoder's dimensions and remain alive during
        // the synchronous call. x264 copies input; output remains valid until its next call.
        let length = unsafe {
            nano_x264_encode(
                self.handle.as_ptr(),
                yuv.y().as_ptr(),
                yuv.u().as_ptr(),
                yuv.v().as_ptr(),
                self.dimensions.0 as c_int,
                pts,
                keyframe.into(),
                &mut bytes,
            )
        };
        if length <= 0 || bytes.is_null() {
            bail!("x264 failed to produce an immediate access unit: {length}");
        }
        // SAFETY: The shim returns the contiguous Annex-B access unit owned by x264;
        // copy it before another encode or reconfiguration can invalidate the pointer.
        Ok(unsafe { std::slice::from_raw_parts(bytes, length as usize) }.to_vec())
    }

    fn apply_bitrate(&mut self, kbps: u32, fps: u16, mode: LatencyMode) -> Result<()> {
        // SAFETY: The handle is exclusively borrowed and the shim consumes only scalars.
        let result = unsafe {
            nano_x264_bitrate(
                self.handle.as_ptr(),
                kbps.try_into().context("x264 bitrate overflows")?,
                fps.into(),
                buffer_frames(mode),
            )
        };
        if result < 0 {
            bail!("x264 VBV update failed: {result}");
        }
        Ok(())
    }

    /// Re-opens the encoder with a different adaptive tier; env-pinned knobs win.
    /// Returns 0 on a re-open, 1 when already there, -2 when adapt is disabled.
    fn adapt(&mut self, tier: u8) -> c_int {
        // SAFETY: The handle is exclusively borrowed and the shim takes one scalar.
        unsafe { nano_x264_adapt(self.handle.as_ptr(), tier.into()) }
    }

    #[cfg(test)]
    fn parameters(&mut self) -> Parameters {
        let mut params = Parameters::default();
        // SAFETY: Parameters mirrors our shim's scalar-only snapshot, not x264_param_t.
        unsafe { nano_x264_parameters(self.handle.as_ptr(), &mut params) };
        params
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        // SAFETY: This is the sole owner; close releases the encoder before its library.
        unsafe { nano_x264_close(self.handle.as_ptr()) };
    }
}

/// Content-driven encoder parameter negotiation. Tier 0 (baseline/hex/subme2)
/// is the measured worst-case-safe configuration; tiers 1-4 spend encode
/// headroom on CABAC/8x8, UMH, a slower preset, and finally fewer slice
/// boundaries plus a longer reference window for fewer bits.
/// The policy only promotes after a sustained easy window and demotes fast
/// under stress — defaults must never regress the hard scene.
struct AdaptPolicy {
    tier: u8,
    encode_ewma_ns: f64,
    bytes_ewma: f64,
    ease_frames: u32,
    stress_frames: u32,
    cooldown: u32,
    switches: u32,
    network_constrained: bool,
}

impl AdaptPolicy {
    const MAX_TIER: u8 = 4;
    /// Frames of sustained ease before each promotion step — cheap tier 1
    /// (CABAC/8x8) engages fast, while the slower/single-slice top tiers stay
    /// careful. ~1s/1.5s/2.5s/4s at 60fps.
    const EASE_FRAMES: [u32; 4] = [60, 90, 150, 240];
    /// Frames needed for the first clearly trivial stream to skip the ladder.
    /// Later tier-0 stays use the normal conservative ease window.
    const INITIAL_TRIVIAL_EASE_FRAMES: u32 = 30;
    /// Consecutive stressed frames before a demotion (~0.33s at 60fps).
    const STRESS_FRAMES: u32 = 20;
    /// Minimum frames between any two tier switches.
    const SWITCH_COOLDOWN: u32 = 60;
    const EWMA_ALPHA: f64 = 0.1;

    /// Returns `Some(tier)` when a switch is warranted; the caller applies it
    /// and reports back through `committed` (success) or `failed`.
    fn observe(
        &mut self,
        encode_ns: u64,
        au_bytes: usize,
        budget_ns: f64,
        cap_bytes: f64,
    ) -> Option<u8> {
        let encode = encode_ns as f64;
        let bytes = au_bytes as f64;
        if self.encode_ewma_ns == 0.0 {
            self.encode_ewma_ns = encode;
            self.bytes_ewma = bytes;
        } else {
            self.encode_ewma_ns += (encode - self.encode_ewma_ns) * Self::EWMA_ALPHA;
            self.bytes_ewma += (bytes - self.bytes_ewma) * Self::EWMA_ALPHA;
        }
        self.cooldown = self.cooldown.saturating_add(1);
        // Hard overload: drop straight to the safe tier.
        if self.tier > 0 && self.encode_ewma_ns > 0.85 * budget_ns {
            return Some(0);
        }
        let byte_stress = self.bytes_ewma > 0.85 * cap_bytes;
        // A full rate-control bucket means two very different things. With
        // network headroom it is content stress, so falling back to cheaper
        // analysis protects fps. When the network itself is the constraint,
        // the bucket is expected to be full; falling back would spend the
        // scarce bytes less efficiently for no latency benefit. CPU overload
        // remains a hard stress signal in both cases.
        if self.encode_ewma_ns > 0.55 * budget_ns || (byte_stress && !self.network_constrained) {
            self.ease_frames = 0;
            self.stress_frames = self.stress_frames.saturating_add(1);
            if self.tier > 0 && self.stress_frames >= Self::STRESS_FRAMES {
                return Some(self.tier - 1);
            }
            return None;
        }
        self.stress_frames = 0;
        self.ease_frames = self.ease_frames.saturating_add(1);
        // Startup is the expensive path: every extra second in tier0 re-encodes
        // trivial scrolling content without CABAC/UMH/long references. Thirty
        // clean deltas are enough evidence when both encode time and produced
        // bytes are far below budget; after any switch, the conservative ladder
        // below prevents oscillation.
        if self.tier == 0
            && self.switches == 0
            && self.ease_frames >= Self::INITIAL_TRIVIAL_EASE_FRAMES
            && self.encode_ewma_ns < 0.12 * budget_ns
            && (self.bytes_ewma < 0.20 * cap_bytes || self.network_constrained)
        {
            return Some(Self::MAX_TIER);
        }
        if self.tier < Self::MAX_TIER
            && self.ease_frames >= Self::EASE_FRAMES[self.tier as usize]
            && self.cooldown >= Self::SWITCH_COOLDOWN
            && self.encode_ewma_ns < 0.30 * budget_ns
            && (self.bytes_ewma < 0.50 * cap_bytes || self.network_constrained)
        {
            // Trivially easy content jumps straight to the most efficient tier —
            // one re-open/IDR, and no seconds spent with short references.
            if self.encode_ewma_ns < 0.12 * budget_ns && self.bytes_ewma < 0.20 * cap_bytes {
                return Some(Self::MAX_TIER);
            }
            return Some(self.tier + 1);
        }
        None
    }

    fn committed(&mut self, tier: u8) {
        self.tier = tier;
        self.ease_frames = 0;
        self.stress_frames = 0;
        self.cooldown = 0;
        self.switches += 1;
    }

    /// A failed shim call gets a quiet period so we don't retry every frame.
    fn failed(&mut self) {
        self.ease_frames = 0;
        self.stress_frames = 0;
        self.cooldown = 0;
    }
}

pub(super) struct X264Encoder {
    active: Active,
    pub(super) bitrate_kbps: u32,
    pub(super) max_fps: u16,
    pub(super) latency_mode: LatencyMode,
    pub(super) color_convert_ns: u64,
    yuv: Option<I420Frame>,
    force_keyframe: bool,
    started: Instant,
    last_pts: i64,
    adapt: AdaptPolicy,
    adapt_enabled: bool,
}

impl X264Encoder {
    pub(super) fn new(
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        dimensions: (u32, u32),
    ) -> Result<Self> {
        Ok(Self {
            active: Active::new(dimensions, bitrate_kbps, max_fps, latency_mode, None)?,
            bitrate_kbps,
            max_fps,
            latency_mode,
            color_convert_ns: 0,
            yuv: None,
            force_keyframe: true,
            started: Instant::now(),
            last_pts: -1,
            adapt: AdaptPolicy {
                tier: 0,
                encode_ewma_ns: 0.0,
                bytes_ewma: 0.0,
                ease_frames: 0,
                stress_frames: 0,
                cooldown: 0,
                switches: 0,
                network_constrained: false,
            },
            adapt_enabled: !matches!(
                std::env::var("NANOCTL_X264_ADAPT").as_deref(),
                Ok("0") | Ok("false")
            ),
        })
    }

    pub(super) fn set_network_constrained(&mut self, constrained: bool) {
        self.adapt.network_constrained = constrained;
    }

    /// Current negotiation tier and how many switches have fired — consumed by
    /// media-smoke for per-second adaptation observability.
    pub(super) fn adapt_state(&self) -> (u8, u32) {
        (self.adapt.tier, self.adapt.switches)
    }

    pub(super) fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    #[cfg(test)]
    fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
        let Some(target) = bitrate_target(
            self.bitrate_kbps,
            estimate_kbps,
            250.min(ceiling_kbps),
            ceiling_kbps,
        ) else {
            return Ok(false);
        };
        self.set_bitrate(target)?;
        Ok(true)
    }

    fn set_bitrate(&mut self, target: u32) -> Result<()> {
        self.active
            .apply_bitrate(target, self.max_fps, self.latency_mode)?;
        self.bitrate_kbps = target;
        Ok(())
    }

    pub(super) fn encode(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
    ) -> Result<EncodedFrame> {
        self.encode_format(image, PixelFormat::Rgba)
    }

    pub(super) fn encode_format(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
        format: PixelFormat,
    ) -> Result<EncodedFrame> {
        self.encode_format_with_constant_mb(image, format, None)
    }

    pub(super) fn encode_format_with_constant_mb(
        &mut self,
        image: &xcap::image::ImageBuffer<
            xcap::image::Rgba<u8>,
            impl std::ops::Deref<Target = [u8]>,
        >,
        format: PixelFormat,
        constant_mb: Option<&[u8]>,
    ) -> Result<EncodedFrame> {
        let dimensions = (image.width(), image.height());
        if self.active.dimensions != dimensions {
            self.active = Active::new(
                dimensions,
                self.bitrate_kbps,
                self.max_fps,
                self.latency_mode,
                None,
            )?;
            self.yuv = None;
            self.force_keyframe = true;
        }
        let color_started = Instant::now();
        let mut fresh_i420 = false;
        if self.yuv.is_none() {
            self.yuv = Some(I420Frame::new(
                dimensions.0 as usize,
                dimensions.1 as usize,
            )?);
            fresh_i420 = true;
        }
        let yuv = self.yuv.as_mut().context("I420 buffer is unavailable")?;
        if !fresh_i420 && let Some(constant_mb) = constant_mb {
            yuv.read_pixels_constant_mb(image.as_raw(), format, constant_mb)?;
        } else {
            yuv.read_pixels(image.as_raw(), format)?;
        }
        self.color_convert_ns += color_started.elapsed().as_nanos() as u64;
        let pts = (self
            .started
            .elapsed()
            .as_micros()
            .min((i64::MAX - 1) as u128) as i64)
            .max(self.last_pts + 1);
        let encode_started = Instant::now();
        let bytes = self.active.encode(yuv, pts, self.force_keyframe)?;
        let encode_ns = encode_started.elapsed().as_nanos() as u64;
        self.last_pts = pts;
        self.force_keyframe = false;
        if self.adapt_enabled {
            let fps = f64::from(self.max_fps.max(1));
            let budget_ns = 1_000_000_000.0 / fps;
            let cap_bytes = f64::from(self.bitrate_kbps) * 125.0 / fps;
            if let Some(target) = self
                .adapt
                .observe(encode_ns, bytes.len(), budget_ns, cap_bytes)
            {
                match self.active.adapt(target) {
                    // 0 = re-opened with new params; 1 = already at the masked target.
                    0 => {
                        self.adapt.committed(target);
                        // A re-open resets the decoder chain — land on a fresh IDR.
                        self.force_keyframe = true;
                        tracing::info!(tier = target, "x264 adaptive tier switch");
                    }
                    1 => {
                        self.adapt.committed(target);
                    }
                    -2 => {
                        self.adapt_enabled = false;
                    }
                    result => {
                        tracing::warn!(result, "x264 adaptive re-open failed");
                        self.adapt.failed();
                    }
                }
            }
        }
        Ok(EncodedFrame {
            bytes,
            width: dimensions.0,
            height: dimensions.1,
        })
    }
}

pub(super) enum ResilientX264 {
    Native(Box<X264Encoder>),
    Fallback(Box<SoftwareEncoder>),
}

impl FrameEncoder for ResilientX264 {
    fn force_keyframe(&mut self) {
        match self {
            Self::Native(encoder) => encoder.force_keyframe(),
            Self::Fallback(encoder) => encoder.force_keyframe(),
        }
    }

    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()> {
        match self {
            Self::Native(encoder) => encoder.set_bitrate(target_kbps),
            Self::Fallback(encoder) => encoder.set_bitrate(target_kbps),
        }
    }

    fn set_network_constrained(&mut self, constrained: bool) {
        if let Self::Native(encoder) = self {
            encoder.set_network_constrained(constrained);
        }
    }

    fn color_convert_ns(&self) -> u64 {
        match self {
            Self::Native(encoder) => encoder.color_convert_ns,
            Self::Fallback(encoder) => encoder.color_convert_ns(),
        }
    }

    fn encode(&mut self, image: &RgbaFrame<'_>) -> Result<EncodedFrame> {
        match self {
            Self::Native(encoder) => match encoder.encode(image) {
                Ok(frame) => Ok(frame),
                Err(error) => {
                    tracing::warn!(%error, "x264 failed; switching session to OpenH264");
                    let fallback = SoftwareEncoder::new(
                        encoder.bitrate_kbps,
                        encoder.max_fps,
                        encoder.latency_mode,
                    )
                    .context("x264 failed and OpenH264 fallback is unavailable")?;
                    *self = Self::Fallback(Box::new(fallback));
                    self.encode(image)
                }
            },
            Self::Fallback(encoder) => encoder.encode(image),
        }
    }

    fn encode_format(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
    ) -> Result<EncodedFrame> {
        match self {
            Self::Native(encoder) => match encoder.encode_format(image, format) {
                Ok(frame) => Ok(frame),
                Err(error) => {
                    tracing::warn!(%error, "x264 failed; switching session to OpenH264");
                    let fallback = SoftwareEncoder::new(
                        encoder.bitrate_kbps,
                        encoder.max_fps,
                        encoder.latency_mode,
                    )
                    .context("x264 failed and OpenH264 fallback is unavailable")?;
                    *self = Self::Fallback(Box::new(fallback));
                    self.encode_format(image, format)
                }
            },
            Self::Fallback(encoder) => encoder.encode_format(image, format),
        }
    }

    fn encode_format_with_constant_mb(
        &mut self,
        image: &RgbaFrame<'_>,
        format: PixelFormat,
        constant_mb: Option<&[u8]>,
    ) -> Result<EncodedFrame> {
        match self {
            Self::Native(encoder) => {
                match encoder.encode_format_with_constant_mb(image, format, constant_mb) {
                    Ok(frame) => Ok(frame),
                    Err(error) => {
                        tracing::warn!(%error, "x264 failed; switching session to OpenH264");
                        let fallback = SoftwareEncoder::new(
                            encoder.bitrate_kbps,
                            encoder.max_fps,
                            encoder.latency_mode,
                        )
                        .context("x264 failed and OpenH264 fallback is unavailable")?;
                        *self = Self::Fallback(Box::new(fallback));
                        self.encode_format(image, format)
                    }
                }
            }
            Self::Fallback(encoder) => encoder.encode_format(image, format),
        }
    }

    fn adapt_state(&self) -> Option<(u8, u32)> {
        match self {
            Self::Native(encoder) => Some(encoder.adapt_state()),
            Self::Fallback(_) => None,
        }
    }

    fn software_backend(&self) -> (&'static str, &'static str) {
        match self {
            Self::Native(_) => ("x264", "software-x264"),
            Self::Fallback(_) => ("OpenH264", "software-openh264"),
        }
    }
}

#[cfg(test)]
#[repr(C)]
#[derive(Default, Debug)]
struct Parameters {
    bitrate: c_int,
    buffer_kbit: c_int,
    crf_milli: c_int,
    qp_max: c_int,
    csp: c_int,
    threads: c_int,
    bframes: c_int,
    lookahead: c_int,
    sync_lookahead: c_int,
    cabac: c_int,
    transform8x8: c_int,
    repeat_headers: c_int,
    annexb: c_int,
    fullrange: c_int,
    primaries: c_int,
    transfer: c_int,
    matrix: c_int,
    chroma_location: c_int,
    keyint: c_int,
    rc_method: c_int,
    me: c_int,
    subme: c_int,
    refs: c_int,
    trellis: c_int,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> xcap::image::RgbaImage {
        xcap::image::RgbaImage::from_pixel(width, height, xcap::image::Rgba([64, 128, 192, 255]))
    }

    fn nals(bytes: &[u8]) -> Vec<&[u8]> {
        openh264::nal_units(bytes).collect()
    }

    fn nal_type(nal: &[u8]) -> u8 {
        let prefix = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
        nal[prefix] & 31
    }

    #[test]
    fn effective_low_latency_crf_vbv_and_vui_parameters_across_modes() {
        for (mode, frames) in [
            (LatencyMode::Responsiveness, 1),
            (LatencyMode::Balanced, 2),
            (LatencyMode::Quality, 4),
        ] {
            // Height 320: x264 clamps slice threads on short frames (~64 rows each).
            let mut encoder = X264Encoder::new(4_000, 60, mode, (128, 320)).unwrap();
            let p = encoder.active.parameters();
            assert_eq!(
                (p.bitrate, p.buffer_kbit),
                (4_000, (4_000 * frames + 59) / 60)
            );
            assert_eq!((p.rc_method, p.crf_milli, p.qp_max), (1, 18_000, 51));
            assert_eq!(p.csp, 2);
            assert!(p.threads >= 2);
            assert_eq!((p.bframes, p.lookahead, p.sync_lookahead), (0, 0, 0));
            assert_eq!((p.cabac, p.transform8x8), (0, 0));
            assert_eq!((p.annexb, p.repeat_headers, p.keyint), (1, 1, 1 << 30));
            assert_eq!(
                (
                    p.fullrange,
                    p.primaries,
                    p.transfer,
                    p.matrix,
                    p.chroma_location
                ),
                (0, 1, 13, 6, 1)
            );
            assert!(!encoder.encode(&image(128, 128)).unwrap().bytes.is_empty());
        }
    }

    #[test]
    fn runtime_resolution_failure_permanently_falls_back_and_retains_controls() {
        let native = X264Encoder::new(4_000, 60, LatencyMode::Balanced, (64, 64)).unwrap();
        let mut backend = ResilientX264::Native(Box::new(native));
        let pixels = image(64, 64);
        let frame = RgbaFrame::from_raw(64, 64, pixels.as_raw().as_slice()).unwrap();
        assert_eq!(backend.software_backend(), ("x264", "software-x264"));
        backend.set_bitrate(8_000).unwrap();
        assert!(!backend.encode(&frame).unwrap().bytes.is_empty());
        let ResilientX264::Native(native) = &mut backend else {
            panic!("expected x264")
        };
        assert_eq!(native.active.parameters().bitrate, 8_000);
        let empty = RgbaFrame::from_raw(0, 0, &[][..]).unwrap();
        assert!(backend.encode(&empty).is_err());
        assert_eq!(
            backend.software_backend(),
            ("OpenH264", "software-openh264")
        );
        assert!(!backend.encode(&frame).unwrap().bytes.is_empty());
        backend.set_bitrate(1_000).unwrap();
        backend.force_keyframe();
        let recovered = backend.encode(&frame).unwrap();
        assert!(crate::codec::is_keyframe(
            crate::codec::VideoCodec::H264,
            &recovered.bytes
        ));
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        assert!(decoder.decode(&recovered.bytes).unwrap().is_some());
    }

    #[test]
    fn immediate_annex_b_baseline_and_requested_idr_include_parameter_sets() {
        let mut encoder = X264Encoder::new(4_000, 60, LatencyMode::Balanced, (64, 64)).unwrap();
        for index in 0..4 {
            if index == 3 {
                encoder.force_keyframe();
            }
            let frame = encoder.encode(&image(64, 64)).unwrap();
            let units = nals(&frame.bytes);
            let types: Vec<_> = units.iter().map(|nal| nal_type(nal)).collect();
            if index == 0 || index == 3 {
                assert!(
                    types.contains(&7) && types.contains(&8) && types.contains(&5),
                    "{types:?}"
                );
                let sps = units.iter().find(|nal| nal_type(nal) == 7).unwrap();
                let offset = if sps.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
                assert_eq!(sps[offset + 1], 66);
                assert_eq!(sps[offset + 2] & 0xc0, 0xc0);
            } else {
                assert!(types.contains(&1) && !types.contains(&5), "{types:?}");
            }
            let mut decoder = openh264::decoder::Decoder::new().unwrap();
            if index == 0 || index == 3 {
                assert!(decoder.decode(&frame.bytes).unwrap().is_some());
            }
        }
    }

    #[test]
    fn adaptive_backoff_and_recovery_reconfigure_vbv_without_changing_crf() {
        let mut encoder = X264Encoder::new(4_000, 60, LatencyMode::Quality, (64, 64)).unwrap();
        encoder.encode(&image(64, 64)).unwrap();
        assert!(encoder.apply_bitrate_estimate(500, 8_000).unwrap());
        encoder.encode(&image(64, 64)).unwrap();
        let p = encoder.active.parameters();
        assert_eq!(p.bitrate, encoder.bitrate_kbps as i32);
        assert!(p.bitrate < 4_000);
        assert_eq!(p.buffer_kbit, (p.bitrate * 4 + 59) / 60);
        assert_eq!((p.crf_milli, p.qp_max), (18_000, 51));
        assert!(
            !encoder
                .apply_bitrate_estimate(encoder.bitrate_kbps, 8_000)
                .unwrap()
        );
        assert!(encoder.apply_bitrate_estimate(8_000, 8_000).unwrap());
        encoder.encode(&image(64, 64)).unwrap();
        assert!(encoder.active.parameters().bitrate > p.bitrate);
        assert!(encoder.encode(&image(64, 64)).is_ok());
    }

    #[test]
    fn adaptive_tiers_reopen_with_cabac_and_umh_then_restore_and_decode() {
        const ME_HEX: i32 = 1;
        const ME_UMH: i32 = 2;
        let mut encoder = X264Encoder::new(4_000, 60, LatencyMode::Balanced, (64, 320)).unwrap();
        let p = encoder.active.parameters();
        assert_eq!((p.cabac, p.transform8x8, p.me, p.subme), (0, 0, ME_HEX, 2));
        assert!(p.threads >= 2);
        let tier0_threads = p.threads;
        assert_eq!(encoder.active.adapt(1), 0);
        let p = encoder.active.parameters();
        assert_eq!((p.cabac, p.transform8x8, p.me, p.subme), (1, 1, ME_HEX, 2));
        assert_eq!(p.threads, 2);
        assert!(tier0_threads >= p.threads);
        assert_eq!(encoder.active.adapt(1), 1);
        assert_eq!(encoder.active.adapt(2), 0);
        assert_eq!(encoder.active.parameters().me, ME_UMH);
        assert_eq!(encoder.active.adapt(3), 0);
        let p = encoder.active.parameters();
        assert_eq!(
            (p.cabac, p.me, p.subme, p.threads, p.refs, p.trellis),
            (1, ME_UMH, 6, 2, 4, 0)
        );
        assert_eq!(encoder.active.adapt(4), 0);
        let p = encoder.active.parameters();
        assert_eq!(
            (p.cabac, p.me, p.subme, p.threads, p.refs, p.trellis),
            (1, ME_UMH, 6, 1, 4, 0)
        );
        assert_eq!(encoder.active.adapt(0), 0);
        let p = encoder.active.parameters();
        assert_eq!((p.cabac, p.transform8x8, p.me), (0, 0, ME_HEX));
        assert!(p.threads >= 2);
        assert_eq!(encoder.active.adapt(9), -1);
        // The stream is still valid after all the re-opens.
        encoder.force_keyframe();
        let frame = encoder.encode(&image(64, 64)).unwrap();
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        assert!(decoder.decode(&frame.bytes).unwrap().is_some());
    }

    #[test]
    fn adapt_policy_promotes_on_ease_demotes_on_stress() {
        let mut policy = AdaptPolicy {
            tier: 0,
            encode_ewma_ns: 0.0,
            bytes_ewma: 0.0,
            ease_frames: 0,
            stress_frames: 0,
            cooldown: 0,
            switches: 0,
            network_constrained: false,
        };
        let budget = 1_000_000_000.0 / 60.0;
        let cap = 4_000.0 * 125.0 / 60.0;
        // Moderately easy frames climb one rung at a time.
        for _ in 0..AdaptPolicy::EASE_FRAMES[0] - 1 {
            assert_eq!(policy.observe(4_500_000, 2_500, budget, cap), None);
        }
        assert_eq!(policy.observe(4_500_000, 2_500, budget, cap), Some(1));
        policy.committed(1);

        // A fresh trivial stream skips the ladder after thirty clean deltas.
        let mut startup = AdaptPolicy {
            tier: 0,
            encode_ewma_ns: 0.0,
            bytes_ewma: 0.0,
            ease_frames: 0,
            stress_frames: 0,
            cooldown: 0,
            switches: 0,
            network_constrained: false,
        };
        for _ in 0..AdaptPolicy::INITIAL_TRIVIAL_EASE_FRAMES - 1 {
            assert_eq!(startup.observe(1_000_000, 500, budget, cap), None);
        }
        assert_eq!(
            startup.observe(1_000_000, 500, budget, cap),
            Some(AdaptPolicy::MAX_TIER)
        );
        // Trivially easy frames jump straight to the most efficient tier.
        for _ in 0..AdaptPolicy::EASE_FRAMES[1] + AdaptPolicy::SWITCH_COOLDOWN {
            policy.observe(1_000_000, 500, budget, cap);
        }
        assert_eq!(policy.observe(1_000_000, 500, budget, cap), Some(4));
        policy.committed(4);
        // Hard overload drops straight to tier 0.
        for _ in 0..40 {
            policy.observe(15_000_000, 500, budget, cap);
        }
        assert_eq!(policy.observe(15_000_000, 500, budget, cap), Some(0));
        policy.committed(0);
        // Bitrate pressure alone demotes one tier at a time (the bytes EWMA
        // needs ~30 frames to cross the cap, then 20 stressed frames).
        policy.tier = 2;
        for _ in 0..80 {
            policy.observe(1_000_000, (cap * 0.9) as usize, budget, cap);
        }
        assert_eq!(
            policy.observe(1_000_000, (cap * 0.9) as usize, budget, cap),
            Some(1)
        );
    }

    #[test]
    fn adapt_policy_network_pressure_does_not_look_like_content_stress() {
        let mut policy = AdaptPolicy {
            tier: 1,
            encode_ewma_ns: 0.0,
            bytes_ewma: 0.0,
            ease_frames: 0,
            stress_frames: 0,
            cooldown: 0,
            switches: 0,
            network_constrained: true,
        };
        let budget = 1_000_000_000.0 / 60.0;
        let cap = 4_000.0 * 125.0 / 60.0;
        // Full-rate frames under a network cap neither demote efficient tiers
        // nor block promotion when the CPU has headroom.
        for _ in 0..AdaptPolicy::EASE_FRAMES[0] + AdaptPolicy::SWITCH_COOLDOWN - 1 {
            assert_ne!(
                policy.observe(4_500_000, (cap * 0.9) as usize, budget, cap),
                Some(0)
            );
        }
        assert_eq!(
            policy.observe(4_500_000, (cap * 0.9) as usize, budget, cap),
            Some(2)
        );

        // Once the network proves headroom, the same full bucket is content
        // stress again and cheap analysis returns after the stress window.
        policy.committed(2);
        policy.network_constrained = false;
        let mut demoted = None;
        for _ in 0..120 {
            if let Some(tier) = policy.observe(4_500_000, (cap * 0.9) as usize, budget, cap) {
                demoted = Some(tier);
                break;
            }
        }
        assert_eq!(demoted, Some(1));
    }

    #[test]
    fn resolution_change_starts_a_fresh_decodable_stream() {
        let mut encoder = X264Encoder::new(4_000, 60, LatencyMode::Balanced, (64, 64)).unwrap();
        encoder.encode(&image(64, 64)).unwrap();
        let frame = encoder.encode(&image(128, 96)).unwrap();
        assert_eq!((frame.width, frame.height), (128, 96));
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        assert_eq!(
            decoder.decode(&frame.bytes).unwrap().unwrap().dimensions(),
            (128, 96)
        );
    }

    #[test]
    fn invalid_dimensions_rates_missing_library_and_mismatched_planes_are_errors() {
        for dimensions in [(0, 64), (64, 0), (63, 64), (64, 63), (u32::MAX - 1, 64)] {
            assert!(Active::new(dimensions, 4_000, 60, LatencyMode::Balanced, None).is_err());
        }
        for (bitrate, fps) in [(0, 60), (4_000, 0)] {
            assert!(Active::new((64, 64), bitrate, fps, LatencyMode::Balanced, None).is_err());
        }
        assert!(
            Active::new(
                (64, 64),
                4_000,
                60,
                LatencyMode::Balanced,
                Some(c"/nanoctl-test/missing-libx264")
            )
            .is_err()
        );
        let mut active = Active::new((64, 64), 4_000, 60, LatencyMode::Balanced, None).unwrap();
        assert!(
            active
                .encode(&I420Frame::new(32, 32).unwrap(), 0, true)
                .is_err()
        );
        assert!(active.apply_bitrate(0, 60, LatencyMode::Balanced).is_err());
    }

    #[tokio::test]
    async fn dropped_encoded_reference_is_rejected_and_requested_idr_repairs_decode() {
        use super::super::{EncodedFrameSlot, ReferenceRecovery};

        let mut encoder = X264Encoder::new(4_000, 60, LatencyMode::Balanced, (128, 96)).unwrap();
        let moving = |offset| {
            xcap::image::RgbaImage::from_fn(128, 96, |x, y| {
                let x = (x + offset) % 128;
                let v = ((x * 53 + y * 97 + x * y) % 256) as u8;
                xcap::image::Rgba([v, v, v, 255])
            })
        };
        let first = encoder.encode(&moving(0)).unwrap();
        let skipped = encoder.encode(&moving(4)).unwrap();
        let delta = encoder.encode(&moving(8)).unwrap();
        let mut complete = openh264::decoder::Decoder::new().unwrap();
        complete.decode(&first.bytes).unwrap();
        complete.decode(&skipped.bytes).unwrap();
        let correct = complete.decode(&delta.bytes).unwrap().unwrap().y().to_vec();
        let mut lost = openh264::decoder::Decoder::new().unwrap();
        lost.decode(&first.bytes).unwrap();
        let damaged = lost
            .decode(&delta.bytes)
            .ok()
            .flatten()
            .map(|frame| frame.y().to_vec());
        assert_ne!(damaged, Some(correct));

        let slot = EncodedFrameSlot::new();
        slot.replace(skipped);
        slot.replace(delta);
        let (frame, dropped) = slot.take().await.unwrap();
        assert_eq!(dropped, 1);
        let (requests, mut observed) = tokio::sync::watch::channel(0_u64);
        let mut recovery = ReferenceRecovery::default();
        assert!(!recovery.admit(&frame.bytes, dropped, &requests));
        assert!(observed.has_changed().unwrap());
        observed.borrow_and_update();
        encoder.force_keyframe();
        let idr = encoder.encode(&moving(8)).unwrap();
        assert!(recovery.admit(&idr.bytes, 0, &requests));
        let repaired = lost.decode(&idr.bytes).unwrap().unwrap().y().to_vec();
        let mut fresh = openh264::decoder::Decoder::new().unwrap();
        assert_eq!(repaired, fresh.decode(&idr.bytes).unwrap().unwrap().y());
    }
}
