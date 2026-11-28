//! Linux VA-API H.264 encoder.
//!
//! Captured RGBA frames are converted to NV12 in process and uploaded into VA surfaces.
//! The codec and reference-frame machinery comes from the ChromeOS `cros-codecs` implementation;
//! encoded frames never pass through an external process or media server.

use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
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
    Display, DrmDeviceIterator, Image, Surface, SurfaceMemoryDescriptor, UsageHint, VA_FOURCC_NV12,
    VA_RT_FORMAT_YUV420,
    VAEntrypoint::{VAEntrypointEncSlice, VAEntrypointEncSliceLP},
    VAProfile::VAProfileH264ConstrainedBaseline,
};

use crate::config::LatencyMode;
use crate::media::{EncodedFrame, FrameEncoder};

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
    nv12: Arc<Vec<u8>>,
}

impl LinuxEncoder {
    pub fn new(
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        (width, height): (u32, u32),
    ) -> Result<Self> {
        let resolution = Resolution { width, height };
        let mut last_error = anyhow!("no accessible VA-API render device");
        for device in DrmDeviceIterator::default() {
            let display = match Display::open_drm_display(&device) {
                Ok(display) => display,
                Err(error) => {
                    last_error = anyhow!("cannot open {device:?}: {error}");
                    continue;
                }
            };
            let mut encoder = Self {
                display,
                active: None,
                bitrate_kbps,
                max_fps,
                latency_mode,
                force_keyframe: true,
                timestamp: 0,
                nv12: Arc::new(Vec::new()),
            };
            match encoder.create_active(resolution) {
                Ok(active) => {
                    encoder.active = Some(active);
                    return Ok(encoder);
                }
                Err(error) => last_error = error.context(format!("{device:?}")),
            }
        }
        Err(last_error)
    }
}

impl FrameEncoder for LinuxEncoder {
    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()> {
        self.bitrate_kbps = target_kbps;
        let tunings = self.tunings();
        if let Some(active) = &mut self.active {
            active
                .encoder
                .tune(tunings)
                .map_err(|error| anyhow!("VA-API bitrate update failed: {error}"))?;
        };
        Ok(())
    }

    fn encode(&mut self, image: &crate::media::RgbaFrame<'_>) -> Result<EncodedFrame> {
        let resolution = Resolution {
            width: image.width(),
            height: image.height(),
        };
        let recreate = self
            .active
            .as_ref()
            .is_none_or(|active| active.layout.size != resolution);
        if recreate {
            // Drop the old VA surface set before opening a replacement; a number of drivers only
            // expose one encode context per process.
            self.active.take();
            self.active = Some(self.create_active(resolution)?);
            self.force_keyframe = true;
        }

        if Arc::get_mut(&mut self.nv12).is_none() {
            self.nv12 = Arc::new(Vec::new());
        }
        rgba_to_nv12(
            image.as_raw(),
            resolution.width,
            resolution.height,
            Arc::get_mut(&mut self.nv12).expect("VA-API frame buffer is uniquely owned"),
        )?;
        let active = self
            .active
            .as_mut()
            .context("VA-API encoder was not initialized")?;
        let frame = CpuNv12Frame {
            resolution,
            bytes: Arc::clone(&self.nv12),
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
                width: image.width(),
                height: image.height(),
            })
            .context("blocking VA-API encode returned no access unit")
    }
}

impl LinuxEncoder {
    fn create_active(&self, resolution: Resolution) -> Result<ActiveEncoder> {
        let entrypoints = self
            .display
            .query_config_entrypoints(VAProfileH264ConstrainedBaseline)
            .context("cannot query VA-API H.264 entrypoints")?;
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
        let level = if resolution.width > 1_920 || resolution.height > 1_080 || self.max_fps > 60 {
            Level::L5_2
        } else {
            Level::L4_2
        };
        let config = EncoderConfig {
            resolution,
            profile: Profile::Baseline,
            level,
            // Use the largest low-delay prediction window the encoder model
            // exposes: recovery keyframes are already forced on loss, so a two-
            // second periodic IDR cadence is pure overhead.
            pred_structure: PredictionStructure::LowDelay { limit: u16::MAX },
            initial_tunings: self.tunings(),
        };
        let mut last_error = anyhow!("VA-API driver has no constrained-baseline H.264 entrypoint");
        for (entrypoint, low_power) in [
            (VAEntrypointEncSliceLP, true),
            (VAEntrypointEncSlice, false),
        ] {
            if entrypoints.contains(&entrypoint) {
                match VaEncoder::new_vaapi(
                    Rc::clone(&self.display),
                    config.clone(),
                    layout.format.0,
                    resolution,
                    low_power,
                    BlockingMode::Blocking,
                ) {
                    Ok(encoder) => return Ok(ActiveEncoder { encoder, layout }),
                    Err(error) => {
                        last_error =
                            anyhow!("cannot initialize VA-API H.264 ({entrypoint:?}): {error}")
                    }
                }
            }
        }
        Err(last_error)
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
    bytes: Arc<Vec<u8>>,
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
    crate::nv12::pixels_to_nv12(rgba, width, height, crate::media::PixelFormat::Rgba, output)
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
