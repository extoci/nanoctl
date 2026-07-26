use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result, bail};
use apple_cf::iosurface::IOSurface;
use videotoolbox::compression::{CompressionSession, ProfileLevel};
use videotoolbox::session::Codec;

use crate::config::LatencyMode;
use crate::media::{EncodedFrame, avcc_to_annex_b, bitrate_target};

const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetFormatDescription(sample_buffer: *mut c_void) -> *mut c_void;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        description: *mut c_void,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> i32;
}

pub struct MacOsEncoder {
    session: Option<CompressionSession>,
    surface: Option<IOSurface>,
    dimensions: (u32, u32),
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
    frame_index: i64,
    restart_session: bool,
}

impl MacOsEncoder {
    pub fn new(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Self> {
        // Probe eagerly so `hardware` fails before capture begins. The selected display dimensions
        // replace this small session on the first frame.
        create_session(16, 16, bitrate_kbps, max_fps, latency_mode)?;
        Ok(Self {
            session: None,
            surface: None,
            dimensions: (0, 0),
            bitrate_kbps,
            max_fps,
            latency_mode,
            frame_index: 0,
            restart_session: true,
        })
    }

    pub fn force_keyframe(&mut self) {
        // A new VTCompressionSession guarantees that the next frame begins a fresh GOP.
        self.restart_session = true;
    }

    pub fn apply_bitrate_estimate(
        &mut self,
        estimate_kbps: u32,
        ceiling_kbps: u32,
    ) -> Result<bool> {
        let floor_kbps = 250.min(ceiling_kbps);
        let Some(target_kbps) =
            bitrate_target(self.bitrate_kbps, estimate_kbps, floor_kbps, ceiling_kbps)
        else {
            return Ok(false);
        };
        self.bitrate_kbps = target_kbps;
        self.restart_session = true;
        Ok(true)
    }

    pub fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        let dimensions = image.dimensions();
        if dimensions.0 == 0 || dimensions.1 == 0 || dimensions.0 % 2 != 0 || dimensions.1 % 2 != 0
        {
            bail!("VideoToolbox input dimensions must be non-zero and even");
        }
        if self.restart_session || self.dimensions != dimensions {
            self.session = Some(create_session(
                dimensions.0,
                dimensions.1,
                self.bitrate_kbps,
                self.max_fps,
                self.latency_mode,
            )?);
            self.surface = Some(
                IOSurface::create(dimensions.0 as usize, dimensions.1 as usize, BGRA, 4)
                    .context("cannot allocate VideoToolbox IOSurface")?,
            );
            self.dimensions = dimensions;
            self.frame_index = 0;
            self.restart_session = false;
        }
        let surface = self
            .surface
            .as_ref()
            .context("VideoToolbox surface is unavailable")?;
        copy_rgba_to_bgra_surface(image, surface)?;
        let encoded = self
            .session
            .as_ref()
            .context("VideoToolbox session is unavailable")?
            .encode(surface, (self.frame_index, i32::from(self.max_fps)))
            .context("VideoToolbox H.264 encode failed")?;
        self.frame_index = self.frame_index.saturating_add(1);
        if encoded.data.is_empty() {
            return Ok(EncodedFrame {
                bytes: Vec::new(),
                width: image.width(),
                height: image.height(),
            });
        }
        let (parameter_sets, nal_length_size) = h264_format(encoded.cm_sample_buffer_ptr().cast())?;
        let parameter_refs = parameter_sets.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let bytes = avcc_to_annex_b(&encoded.data, nal_length_size, &parameter_refs)?;
        Ok(EncodedFrame {
            bytes,
            width: image.width(),
            height: image.height(),
        })
    }
}

fn create_session(
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
) -> Result<CompressionSession> {
    let quality = match latency_mode {
        LatencyMode::Responsiveness => 0.45,
        LatencyMode::Balanced => 0.65,
        LatencyMode::Quality => 0.85,
    };
    let session = CompressionSession::builder(width as i32, height as i32, Codec::H264)
        .with_real_time(true)
        .with_allow_frame_reordering(false)
        .with_average_bit_rate(
            bitrate_kbps
                .saturating_mul(1_000)
                .try_into()
                .context("VideoToolbox bitrate exceeds i32")?,
        )
        .with_expected_frame_rate(f64::from(max_fps))
        .with_max_keyframe_interval(i32::from(max_fps) * 2)
        .with_quality(quality)
        .with_profile_level(ProfileLevel::H264ConstrainedBaselineAutoLevel)
        .build()
        .context("cannot create real-time VideoToolbox H.264 session")?;
    // SAFETY: the key is a process-lifetime CoreFoundation constant documented for compression
    // sessions, and `copy_property` retains the returned value before exposing it.
    let hardware = unsafe {
        session.copy_property(
            videotoolbox::ffi::kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
        )
    }
    .context("cannot query VideoToolbox hardware acceleration")?
    .is_some_and(|value| {
        // SAFETY: kCFBooleanTrue is a process-lifetime singleton used for pointer identity.
        value.as_ptr()
            == unsafe {
                videotoolbox::ffi::kCFBooleanTrue
                    .cast_mut()
                    .cast::<c_void>()
            }
    });
    if !hardware {
        bail!("VideoToolbox selected a software encoder");
    }
    Ok(session)
}

fn copy_rgba_to_bgra_surface(image: &xcap::image::RgbaImage, surface: &IOSurface) -> Result<()> {
    let width_bytes = image.width() as usize * 4;
    let mut guard = surface
        .lock_read_write()
        .map_err(|status| anyhow::anyhow!("IOSurface lock failed: {status}"))?;
    let bytes_per_row = guard.bytes_per_row();
    if bytes_per_row < width_bytes {
        bail!("IOSurface row is smaller than the captured image");
    }
    let destination = guard.as_slice_mut().context("IOSurface is not writable")?;
    for (source_row, destination_row) in image
        .as_raw()
        .chunks_exact(width_bytes)
        .zip(destination.chunks_exact_mut(bytes_per_row))
    {
        for (source, destination) in source_row
            .chunks_exact(4)
            .zip(destination_row[..width_bytes].chunks_exact_mut(4))
        {
            destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
    }
    Ok(())
}

fn h264_format(sample_buffer: *mut c_void) -> Result<(Vec<Vec<u8>>, usize)> {
    if sample_buffer.is_null() {
        bail!("VideoToolbox returned no sample buffer");
    }
    // SAFETY: `sample_buffer` is retained by `EncodedFrame` for this entire function call.
    let description = unsafe { CMSampleBufferGetFormatDescription(sample_buffer) };
    if description.is_null() {
        bail!("VideoToolbox returned no H.264 format description");
    }
    let mut first = ptr::null();
    let mut first_size = 0_usize;
    let mut count = 0_usize;
    let mut nal_length_size = 0_i32;
    // SAFETY: the retained format description owns the returned parameter-set memory, and every
    // out-pointer below is valid for the duration of the call.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            description,
            0,
            &mut first,
            &mut first_size,
            &mut count,
            &mut nal_length_size,
        )
    };
    if status != 0 || first.is_null() || first_size == 0 || !(1..=4).contains(&nal_length_size) {
        bail!("VideoToolbox H.264 format description is invalid ({status})");
    }
    if count == 0 || count > 8 {
        bail!("VideoToolbox returned an invalid H.264 parameter-set count");
    }
    let mut parameter_sets = Vec::with_capacity(count);
    for index in 0..count {
        let mut pointer = ptr::null();
        let mut size = 0_usize;
        // SAFETY: as above; the pointer is copied into an owned Vec before EncodedFrame is dropped.
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                description,
                index,
                &mut pointer,
                &mut size,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 || pointer.is_null() || size == 0 || size > 64 * 1024 {
            bail!("VideoToolbox H.264 parameter set {index} is invalid ({status})");
        }
        // SAFETY: VideoToolbox returned a non-null pointer with the checked bounded `size`, and the
        // format description remains retained while this slice is copied.
        parameter_sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
    }
    Ok((parameter_sets, nal_length_size as usize))
}
