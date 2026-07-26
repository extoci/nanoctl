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

use std::sync::Arc;
use std::time::Duration;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

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

    pub fn next_access_unit(&mut self, max_width: u32, max_height: u32) -> Result<EncodedFrame> {
        let source = self
            .monitor
            .capture_image()
            .context("display capture failed")?;
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

pub fn spawn_video(
    track: Arc<TrackLocalStaticSample>,
    max_bitrate_kbps: u32,
    max_fps: u16,
    max_width: u32,
    max_height: u32,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<EncodedFrame>(1);
        let producer = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut encoder = CaptureEncoder::primary(max_bitrate_kbps, max_fps)?;
            while sender
                .blocking_send(encoder.next_access_unit(max_width, max_height)?)
                .is_ok()
            {}
            Ok(())
        });
        let duration = Duration::from_secs_f64(1.0 / f64::from(max_fps));
        while let Some(frame) = receiver.recv().await {
            track
                .write_sample(&Sample {
                    data: frame.bytes.into(),
                    duration,
                    ..Default::default()
                })
                .await?;
        }
        producer.await??;
        Ok(())
    })
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
    use super::fit_dimensions;

    #[test]
    fn preserves_aspect_ratio_and_even_dimensions() {
        assert_eq!(fit_dimensions(3840, 2160, 1920, 1080), (1920, 1080));
        let (width, height) = fit_dimensions(1921, 1081, 1920, 1080);
        assert_eq!(width % 2, 0);
        assert_eq!(height % 2, 0);
    }
}
