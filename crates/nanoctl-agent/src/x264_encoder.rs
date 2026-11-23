use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use openh264::formats::YUVSource;

#[cfg(test)]
use super::bitrate_target;
use super::{EncodedFrame, FrameEncoder, I420Frame, RgbaFrame, SoftwareEncoder};
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
        })
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
        if self.yuv.is_none() {
            self.yuv = Some(I420Frame::new(
                dimensions.0 as usize,
                dimensions.1 as usize,
            )?);
        }
        let yuv = self.yuv.as_mut().context("I420 buffer is unavailable")?;
        yuv.read_rgba(image.as_raw())?;
        self.color_convert_ns += color_started.elapsed().as_nanos() as u64;
        let pts = (self
            .started
            .elapsed()
            .as_micros()
            .min((i64::MAX - 1) as u128) as i64)
            .max(self.last_pts + 1);
        let bytes = self.active.encode(yuv, pts, self.force_keyframe)?;
        self.last_pts = pts;
        self.force_keyframe = false;
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
            let mut encoder = X264Encoder::new(4_000, 60, mode, (128, 128)).unwrap();
            let p = encoder.active.parameters();
            assert_eq!(
                (p.bitrate, p.buffer_kbit),
                (4_000, (4_000 * frames + 59) / 60)
            );
            assert_eq!((p.rc_method, p.crf_milli, p.qp_max), (1, 12_000, 51));
            assert_eq!((p.csp, p.threads), (2, 2));
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
        assert!(super::super::is_idr(&recovered.bytes));
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
        assert_eq!((p.crf_milli, p.qp_max), (12_000, 51));
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
