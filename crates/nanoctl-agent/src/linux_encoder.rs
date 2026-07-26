//! Linux VA-API H.264 encoder.
//!
//! Captured RGBA frames are converted to NV12 in process and uploaded into VA surfaces.
//! The codec and reference-frame machinery comes from the ChromeOS `cros-codecs` implementation;
//! encoded frames never pass through an external process or media server.

use std::fmt;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use cros_codecs::backend::vaapi::encoder::VaapiBackend;
use cros_codecs::codec::h264::parser::{Level, Profile};
use cros_codecs::encoder::h264::EncoderConfig;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::{
    FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder,
};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution};
use cros_libva::{
    Display, Image, Surface, SurfaceMemoryDescriptor, UsageHint, VA_FOURCC_NV12,
    VA_RT_FORMAT_YUV420,
    VAEntrypoint::{VAEntrypointEncSlice, VAEntrypointEncSliceLP},
    VAProfile::VAProfileH264ConstrainedBaseline,
};

use crate::config::LatencyMode;
use crate::media::{EncodedFrame, bitrate_target};

type VaEncoder = StatelessEncoder<CpuNv12Frame, VaapiBackend<(), Surface<()>>>;

struct ActiveEncoder {
    encoder: VaEncoder,
    layout: FrameLayout,
}

pub struct LinuxEncoder {
    display: Rc<Display>,
    active: Option<ActiveEncoder>,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
    force_keyframe: bool,
    timestamp: u64,
    nv12: Vec<u8>,
}

impl LinuxEncoder {
    pub fn new(bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Self> {
        let display = Display::open().context("cannot open a VA-API render device")?;
        let entrypoints = display
            .query_config_entrypoints(VAProfileH264ConstrainedBaseline)
            .context("cannot query VA-API H.264 capabilities")?;
        if !entrypoints.contains(&VAEntrypointEncSlice)
            && !entrypoints.contains(&VAEntrypointEncSliceLP)
        {
            bail!("VA-API driver has no constrained-baseline H.264 entrypoint");
        }
        let mut encoder = Self {
            display,
            active: None,
            bitrate_kbps,
            max_fps,
            latency_mode,
            force_keyframe: true,
            timestamp: 0,
            nv12: Vec::new(),
        };
        // Construction is also the doctor probe. Build a genuine context and surface set rather
        // than reporting a decode-only profile as encoder availability.
        encoder.active = Some(encoder.create_active(Resolution {
            width: 64,
            height: 64,
        })?);
        Ok(encoder)
    }

    pub fn force_keyframe(&mut self) {
        self.force_keyframe = true;
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
        let tunings = self.tunings();
        if let Some(active) = &mut self.active {
            active
                .encoder
                .tune(tunings)
                .map_err(|error| anyhow!("VA-API bitrate update failed: {error}"))?;
        }
        Ok(true)
    }

    pub fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        let resolution = Resolution {
            width: image.width(),
            height: image.height(),
        };
        let recreate = self
            .active
            .as_ref()
            .is_none_or(|active| active.layout.size != resolution);
        if recreate {
            self.active = Some(self.create_active(resolution)?);
            self.force_keyframe = true;
        }

        rgba_to_nv12(
            image.as_raw(),
            resolution.width,
            resolution.height,
            &mut self.nv12,
        )?;
        let active = self
            .active
            .as_mut()
            .context("VA-API encoder was not initialized")?;
        let frame = CpuNv12Frame {
            resolution,
            bytes: self.nv12.clone(),
        };
        let metadata = FrameMetadata {
            timestamp: self.timestamp,
            layout: active.layout.clone(),
            force_keyframe: std::mem::take(&mut self.force_keyframe),
        };
        self.timestamp = self.timestamp.wrapping_add(1);
        active
            .encoder
            .encode(metadata, frame)
            .map_err(|error| anyhow!("VA-API H.264 encode failed: {error}"))?;
        active
            .encoder
            .poll()
            .map_err(|error| anyhow!("VA-API H.264 output failed: {error}"))?
            .map(|coded| EncodedFrame {
                bytes: coded.bitstream,
            })
            .context("blocking VA-API encode returned no access unit")
    }

    fn create_active(&self, resolution: Resolution) -> Result<ActiveEncoder> {
        let entrypoints = self
            .display
            .query_config_entrypoints(VAProfileH264ConstrainedBaseline)
            .context("cannot query VA-API H.264 entrypoints")?;
        let low_power = entrypoints.contains(&VAEntrypointEncSliceLP);
        let layout = FrameLayout {
            format: (Fourcc::from(b"NV12"), 0),
            size: resolution,
            planes: vec![
                PlaneLayout {
                    buffer_index: 0,
                    offset: 0,
                    stride: resolution.width as usize,
                },
                PlaneLayout {
                    buffer_index: 0,
                    offset: (resolution.width * resolution.height) as usize,
                    stride: resolution.width as usize,
                },
            ],
        };
        let config = EncoderConfig {
            resolution,
            profile: Profile::Baseline,
            level: Level::L4,
            pred_structure: PredictionStructure::LowDelay {
                limit: self.max_fps.saturating_mul(2).max(1),
            },
            initial_tunings: self.tunings(),
        };
        let encoder = VaEncoder::new_vaapi(
            Rc::clone(&self.display),
            config,
            layout.format.0,
            resolution,
            low_power,
            BlockingMode::Blocking,
        )
        .map_err(|error| anyhow!("cannot initialize VA-API H.264 encoder: {error}"))?;
        Ok(ActiveEncoder { encoder, layout })
    }

    fn tunings(&self) -> Tunings {
        let (min_quality, max_quality) = match self.latency_mode {
            LatencyMode::Responsiveness => (20, 42),
            LatencyMode::Balanced => (16, 40),
            LatencyMode::Quality => (10, 36),
        };
        Tunings {
            rate_control: RateControl::ConstantBitrate(u64::from(self.bitrate_kbps) * 1_000),
            framerate: u32::from(self.max_fps),
            min_quality,
            max_quality,
        }
    }
}

struct CpuNv12Frame {
    resolution: Resolution,
    bytes: Vec<u8>,
}

impl fmt::Debug for CpuNv12Frame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpuNv12Frame")
            .field("resolution", &self.resolution)
            .field("byte_length", &self.bytes.len())
            .finish()
    }
}

struct CpuReadMapping<'a>(&'a CpuNv12Frame);

impl<'a> ReadMapping<'a> for CpuReadMapping<'a> {
    fn get(&self) -> Vec<&[u8]> {
        let luma = (self.0.resolution.width * self.0.resolution.height) as usize;
        vec![&self.0.bytes[..luma], &self.0.bytes[luma..]]
    }
}

impl VideoFrame for CpuNv12Frame {
    type MemDescriptor = ();
    type NativeHandle = Surface<()>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let luma = (self.resolution.width * self.resolution.height) as usize;
        vec![luma, luma / 2]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        vec![self.resolution.width as usize; 2]
    }

    fn map<'a>(&'a self) -> std::result::Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Ok(Box::new(CpuReadMapping(self)))
    }

    fn map_mut<'a>(&'a mut self) -> std::result::Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Err("nanoctl VA-API input frames are immutable".to_owned())
    }

    fn to_native_handle(
        &self,
        display: &Rc<Display>,
    ) -> std::result::Result<Self::NativeHandle, String> {
        let mut surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(VA_FOURCC_NV12),
                self.resolution.width,
                self.resolution.height,
                Some(UsageHint::USAGE_HINT_ENCODER),
                vec![()],
            )
            .map_err(|error| error.to_string())?;
        let surface = surfaces.pop().ok_or("VA-API returned no input surface")?;
        upload_nv12_img(
            display,
            &surface,
            self.resolution.width,
            self.resolution.height,
            &self.bytes,
        )
        .map_err(|error| error.to_string())?;
        Ok(surface)
    }
}

fn upload_nv12_img<M: SurfaceMemoryDescriptor>(
    display: &Rc<Display>,
    surface: &Surface<M>,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<()> {
    let image_format = display
        .query_image_formats()
        .context("cannot query VA-API image formats")?
        .into_iter()
        .find(|format| format.fourcc == VA_FOURCC_NV12)
        .context("VA-API driver cannot map NV12 images")?;
    let mut image = Image::create_from(surface, image_format, surface.size(), surface.size())
        .context("cannot map VA-API input surface")?;
    let descriptor = *image.image();
    let destination = image.as_mut();
    let width = width as usize;
    let height = height as usize;
    for row in 0..height {
        let source = &data[row * width..(row + 1) * width];
        let offset = descriptor.offsets[0] as usize + row * descriptor.pitches[0] as usize;
        destination
            .get_mut(offset..offset + width)
            .context("VA-API luma plane is smaller than declared")?
            .copy_from_slice(source);
    }
    let source_chroma = width * height;
    for row in 0..height / 2 {
        let source = &data[source_chroma + row * width..source_chroma + (row + 1) * width];
        let offset = descriptor.offsets[1] as usize + row * descriptor.pitches[1] as usize;
        destination
            .get_mut(offset..offset + width)
            .context("VA-API chroma plane is smaller than declared")?
            .copy_from_slice(source);
    }
    surface
        .sync()
        .context("VA-API input upload did not complete")?;
    Ok(())
}

fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32, output: &mut Vec<u8>) -> Result<()> {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        bail!("NV12 dimensions must be non-zero and even");
    }
    let pixels = width
        .checked_mul(height)
        .context("frame dimensions overflow")?;
    if rgba.len() != pixels.checked_mul(4).context("RGBA frame size overflow")? {
        bail!("RGBA frame length does not match its dimensions");
    }
    output.clear();
    output.resize(pixels + pixels / 2, 0);
    for row in 0..height {
        for column in 0..width {
            let offset = (row * width + column) * 4;
            let r = i32::from(rgba[offset]);
            let g = i32::from(rgba[offset + 1]);
            let b = i32::from(rgba[offset + 2]);
            output[row * width + column] = ((66 * r + 129 * g + 25 * b + 128) >> 8)
                .saturating_add(16)
                .clamp(0, 255) as u8;
        }
    }
    let uv_offset = pixels;
    for row in (0..height).step_by(2) {
        for column in (0..width).step_by(2) {
            let mut r = 0_i32;
            let mut g = 0_i32;
            let mut b = 0_i32;
            for y in 0..2 {
                for x in 0..2 {
                    let offset = ((row + y) * width + column + x) * 4;
                    r += i32::from(rgba[offset]);
                    g += i32::from(rgba[offset + 1]);
                    b += i32::from(rgba[offset + 2]);
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let chroma = uv_offset + (row / 2) * width + column;
            output[chroma] = ((-38 * r - 74 * g + 112 * b + 128) >> 8)
                .saturating_add(128)
                .clamp(0, 255) as u8;
            output[chroma + 1] = ((112 * r - 94 * g - 18 * b + 128) >> 8)
                .saturating_add(128)
                .clamp(0, 255) as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::rgba_to_nv12;

    #[test]
    fn converts_black_and_white_to_limited_range_nv12() {
        let mut output = Vec::new();
        rgba_to_nv12(
            &[
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
            2,
            2,
            &mut output,
        )
        .unwrap();
        assert_eq!(&output[..4], &[16, 235, 16, 235]);
        assert_eq!(&output[4..], &[128, 128]);
    }

    #[test]
    fn rejects_invalid_buffers_and_dimensions() {
        let mut output = Vec::new();
        assert!(rgba_to_nv12(&[], 1, 2, &mut output).is_err());
        assert!(rgba_to_nv12(&[0; 4], 2, 2, &mut output).is_err());
    }
}
