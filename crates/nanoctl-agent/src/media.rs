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
use openh264::formats::{RgbSliceU8, YUVBuffer};
use xcap::Monitor;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::error::TrySendError;
use webrtc::media::Sample;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use crate::config::LatencyMode;

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
}

pub struct CaptureEncoder {
    // Keeping the recorder alive preserves one native capture session instead of rebuilding the
    // OS capture pipeline for every frame.
    recorder: xcap::VideoRecorder,
    latest_frame: Arc<LatestFrame>,
    encoder: Encoder,
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
    rgb: Vec<u8>,
}

impl CaptureEncoder {
    pub fn primary(max_bitrate_kbps: u32, max_fps: u16, latency_mode: LatencyMode) -> Result<Self> {
        let monitors = Monitor::all().context("screen capture is unavailable")?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .cloned()
            .context("no display is available")?;
        let encoder = create_encoder(max_bitrate_kbps, max_fps, latency_mode)?;
        let (recorder, latest_frame) = start_capture(&monitor)?;
        Ok(Self {
            recorder,
            latest_frame,
            encoder,
            bitrate_kbps: max_bitrate_kbps,
            max_fps,
            latency_mode,
            rgb: Vec::new(),
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
        self.encoder.force_intra_frame();
        Ok(())
    }

    pub fn apply_bitrate_estimate(&mut self, estimate_kbps: u32, ceiling_kbps: u32) -> Result<bool> {
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

fn bitrate_target(current: u32, estimate: u32, floor: u32, ceiling: u32) -> Option<u32> {
    let target = estimate.clamp(floor, ceiling);
    let target = u64::from(target);
    let current = u64::from(current);
    if target * 100 <= current * 80 || target * 100 >= current * 125 {
        Some(target as u32)
    } else {
        None
    }
}

impl Drop for CaptureEncoder {
    fn drop(&mut self) {
        let _ = self.recorder.stop();
    }
}

pub struct EncodedFrame {
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct VideoQuality {
    pub max_bitrate_kbps: u32,
    pub max_fps: u16,
    pub max_width: u32,
    pub max_height: u32,
    pub latency_mode: LatencyMode,
}

pub fn spawn_video(
    track: Arc<TrackLocalStaticSample>,
    keyframe_requests: tokio::sync::watch::Receiver<u64>,
    bitrate_estimate_kbps: tokio::sync::watch::Receiver<u32>,
    display_selection: tokio::sync::watch::Receiver<String>,
    quality: VideoQuality,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<EncodedFrame>(1);
        let producer = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut encoder = CaptureEncoder::primary(
                quality.max_bitrate_kbps,
                quality.max_fps,
                quality.latency_mode,
            )?;
            let mut keyframe_requests = keyframe_requests;
            let mut bitrate_estimate_kbps = bitrate_estimate_kbps;
            let mut display_selection = display_selection;
            let frame_interval = Duration::from_secs_f64(1.0 / f64::from(quality.max_fps));
            let mut next_frame = Instant::now();
            loop {
                let now = Instant::now();
                if now < next_frame {
                    std::thread::sleep(next_frame - now);
                }
                // Advance from the actual clock when capture/encode overruns. This prevents a burst
                // of catch-up frames after a slow encode or a suspended machine.
                next_frame = Instant::now() + frame_interval;
                if keyframe_requests.has_changed().unwrap_or(false) {
                    keyframe_requests.borrow_and_update();
                    encoder.encoder.force_intra_frame();
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
                let frame =
                    encoder.next_access_unit(quality.max_width, quality.max_height)?;
                match sender.try_send(frame) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Closed(_)) => break,
                }
            }
            Ok(())
        });
        let duration = Duration::from_secs_f64(1.0 / f64::from(quality.max_fps));
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
    use super::{LatestFrame, bitrate_target, fit_dimensions};
    use std::time::Duration;

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
}
