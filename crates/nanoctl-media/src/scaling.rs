use rayon::prelude::*;

use crate::types::{MediaError, PixelFormat, RawFrame};

/// Fits source dimensions inside the configured bounds without upscaling, rounding
/// down to even dimensions because every v4 encoder currently requires them.
pub(crate) fn fit_dimensions(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (2, 2);
    }
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    let fitted_width = ((f64::from(width) * scale).floor() as u32).max(2) & !1;
    let fitted_height = ((f64::from(height) * scale).floor() as u32).max(2) & !1;
    (fitted_width, fitted_height)
}

#[derive(Clone, Copy)]
struct AxisSample {
    first: usize,
    second: usize,
    second_weight: u32,
}

fn axis_samples(source: u32, target: u32) -> Vec<AxisSample> {
    let scale = f64::from(source) / f64::from(target);
    (0..target)
        .map(|target_index| {
            let center = (f64::from(target_index) + 0.5) * scale - 0.5;
            if center <= 0.0 {
                return AxisSample {
                    first: 0,
                    second: 0,
                    second_weight: 0,
                };
            }
            let first = center.floor() as usize;
            if first >= source as usize - 1 {
                return AxisSample {
                    first: source as usize - 1,
                    second: source as usize - 1,
                    second_weight: 0,
                };
            }
            AxisSample {
                first,
                second: first + 1,
                second_weight: ((center - first as f64) * 256.0).round() as u32,
            }
        })
        .collect()
}

/// Fixed-point bilinear RGBA/BGRA scaler. Channel order does not matter because
/// scaling is component-wise. Row work is parallel; scaling must never become the
/// latency bottleneck that resolution caps were meant to remove.
#[derive(Default)]
pub(crate) struct FrameScaler {
    dimensions: (u32, u32, u32, u32),
    x_samples: Vec<AxisSample>,
    y_samples: Vec<AxisSample>,
}

impl FrameScaler {
    pub(crate) fn scale(
        &mut self,
        frame: RawFrame,
        max_width: u32,
        max_height: u32,
    ) -> Result<RawFrame, MediaError> {
        let (target_width, target_height) =
            fit_dimensions(frame.width, frame.height, max_width, max_height);
        if target_width == frame.width && target_height == frame.height {
            return Ok(frame);
        }
        if !matches!(frame.format, PixelFormat::Rgba32 | PixelFormat::Bgra32) {
            return Err(MediaError::Encode(
                "configured resolution limit requires scaling non-RGBA capture".to_string(),
            ));
        }
        let source_len = (frame.width as usize)
            .checked_mul(frame.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(MediaError::Encode(
                "capture dimensions overflow".to_string(),
            ))?;
        if frame.pixels.len() != source_len {
            return Err(MediaError::Encode(
                "capture frame length does not match its dimensions".to_string(),
            ));
        }
        let dimensions = (frame.width, frame.height, target_width, target_height);
        if self.dimensions != dimensions {
            self.x_samples = axis_samples(frame.width, target_width);
            self.y_samples = axis_samples(frame.height, target_height);
            self.dimensions = dimensions;
        }

        let source_width = frame.width as usize;
        let target_row_bytes = target_width as usize * 4;
        let mut pixels = vec![0; target_row_bytes * target_height as usize];
        pixels
            .par_chunks_exact_mut(target_row_bytes)
            .enumerate()
            .for_each(|(target_y, output_row)| {
                let y = self.y_samples[target_y];
                let first_row = y.first * source_width;
                let second_row = y.second * source_width;
                let wy2 = y.second_weight;
                let wy1 = 256 - wy2;
                for (target_x, x) in self.x_samples.iter().enumerate() {
                    let wx2 = x.second_weight;
                    let wx1 = 256 - wx2;
                    let first = (first_row + x.first) * 4;
                    let first_right = (first_row + x.second) * 4;
                    let second = (second_row + x.first) * 4;
                    let second_right = (second_row + x.second) * 4;
                    for channel in 0..4 {
                        let top = u32::from(frame.pixels[first + channel]) * wx1
                            + u32::from(frame.pixels[first_right + channel]) * wx2;
                        let bottom = u32::from(frame.pixels[second + channel]) * wx1
                            + u32::from(frame.pixels[second_right + channel]) * wx2;
                        output_row[target_x * 4 + channel] =
                            ((top * wy1 + bottom * wy2 + 32_768) >> 16) as u8;
                    }
                }
            });

        let capture_sequence = frame.capture_sequence;
        let mut scaled = RawFrame::new(
            frame.capture_time_us,
            target_width,
            target_height,
            frame.format,
            pixels,
        );
        scaled.capture_sequence = capture_sequence;
        scaled.has_new_damage = frame.has_new_damage;
        Ok(scaled)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameScaler, fit_dimensions};
    use crate::types::{PixelFormat, RawFrame};

    #[test]
    fn fit_dimensions_preserves_aspect_and_even_encoder_dimensions() {
        assert_eq!(fit_dimensions(1600, 1200, 1920, 1080), (1440, 1080));
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(fit_dimensions(3, 3, 2, 2), (2, 2));
    }

    #[test]
    fn scaling_honors_resolution_limit_and_preserves_capture_metadata() {
        let pixels = vec![42; 1600 * 1200 * 4];
        let mut frame = RawFrame::new(123, 1600, 1200, PixelFormat::Bgra32, pixels);
        frame.capture_sequence = 77;
        frame.has_new_damage = false;
        let scaled = FrameScaler::default()
            .scale(frame, 1920, 1080)
            .expect("scaling should succeed");
        assert_eq!((scaled.width, scaled.height), (1440, 1080));
        assert_eq!(scaled.capture_time_us, 123);
        assert_eq!(scaled.capture_sequence, 77);
        assert!(!scaled.has_new_damage);
        assert_eq!(scaled.pixels.len(), 1440 * 1080 * 4);
        assert!(scaled.pixels.iter().all(|pixel| *pixel == 42));
    }

    #[test]
    fn bilinear_scaling_interpolates_channels_independently() {
        let mut pixels = Vec::new();
        for _y in 0..2 {
            for value in [0_u8, 85, 170, 255] {
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let scaled = FrameScaler::default()
            .scale(RawFrame::new(0, 4, 2, PixelFormat::Rgba32, pixels), 2, 2)
            .unwrap();
        assert_eq!((scaled.width, scaled.height), (2, 2));
        assert_eq!(scaled.pixels[2], 43);
        assert_eq!(scaled.pixels[6], 213);
        assert_eq!(scaled.pixels[3], 255);
    }

    #[test]
    fn scaling_rejects_planar_input_instead_of_silently_exceeding_limits() {
        let frame = RawFrame::new(0, 4, 4, PixelFormat::Nv12, vec![0; 24]);
        assert!(FrameScaler::default().scale(frame, 2, 2).is_err());
    }
}
