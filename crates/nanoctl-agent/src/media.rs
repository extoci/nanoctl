//! Cross-platform capture and baseline H.264 encoding.
//!
//! Platform release builds enable the `media` feature. The queue between this producer and WebRTC
//! is deliberately capacity one: a remote desktop should drop an obsolete frame under load rather
//! than preserve it and grow latency.

use anyhow::{Context, Result, bail};
use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, UsageType};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use xcap::Monitor;

pub struct CaptureEncoder {
    monitor: Monitor,
    encoder: Encoder,
    rgb: Vec<u8>,
}

impl CaptureEncoder {
    pub fn primary(max_bitrate_kbps: u32, max_fps: u16) -> Result<Self> {
        let monitors = Monitor::all().context("screen capture is unavailable")?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .cloned()
            .context("no display is available")?;
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(max_bitrate_kbps.saturating_mul(1_000)))
            .max_frame_rate(FrameRate::from_hz(max_fps as f32))
            .usage_type(UsageType::ScreenContentRealTime);
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
            .context("H.264 encoder is unavailable")?;
        Ok(Self {
            monitor,
            encoder,
            rgb: Vec::new(),
        })
    }

    pub fn next_access_unit(&mut self) -> Result<EncodedFrame> {
        let image = self
            .monitor
            .capture_image()
            .context("display capture failed")?;
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            bail!("captured display dimensions must be non-zero and even");
        }
        self.rgb.clear();
        self.rgb.reserve((width as usize) * (height as usize) * 3);
        for pixel in image.as_raw().chunks_exact(4) {
            self.rgb.extend_from_slice(&pixel[..3]);
        }
        let source = RgbSliceU8::new(&self.rgb, (width as usize, height as usize));
        let yuv = YUVBuffer::from_rgb_source(source);
        let stream = self.encoder.encode(&yuv).context("H.264 encode failed")?;
        Ok(EncodedFrame {
            bytes: stream.to_vec(),
            width,
            height,
        })
    }
}

pub struct EncodedFrame {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
