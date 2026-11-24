//! BT.601 limited-range RGBA to NV12 conversion for encoders that take system-memory NV12.

use anyhow::{Context, Result, bail};

/// Below this size a frame converts faster on one thread than it takes to start more.
const PARALLEL_MIN_PIXELS: usize = 640 * 360;
const MAX_THREADS: usize = 8;

pub fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32, output: &mut Vec<u8>) -> Result<()> {
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
    // Every byte is overwritten, so a reused buffer of the right size needs no clearing.
    output.resize(pixels + pixels / 2, 0);
    let (luma, chroma) = output.split_at_mut(pixels);
    let threads = if pixels >= PARALLEL_MIN_PIXELS {
        std::thread::available_parallelism().map_or(1, |count| count.get().min(MAX_THREADS))
    } else {
        1
    };
    let row_pairs_per_band = (height / 2).div_ceil(threads);
    let mut bands = rgba
        .chunks(row_pairs_per_band * 2 * width * 4)
        .zip(luma.chunks_mut(row_pairs_per_band * 2 * width))
        .zip(chroma.chunks_mut(row_pairs_per_band * width));
    let Some(((first_rgba, first_luma), first_chroma)) = bands.next() else {
        return Ok(());
    };
    std::thread::scope(|scope| {
        for ((rgba, luma), chroma) in bands {
            scope.spawn(move || convert_band(rgba, luma, chroma, width));
        }
        convert_band(first_rgba, first_luma, first_chroma, width);
    });
    Ok(())
}

fn convert_band(rgba: &[u8], luma: &mut [u8], chroma: &mut [u8], width: usize) {
    for ((rows, luma_rows), uv) in rgba
        .chunks_exact(width * 8)
        .zip(luma.chunks_exact_mut(width * 2))
        .zip(chroma.chunks_exact_mut(width))
    {
        let (top, bottom) = rows.split_at(width * 4);
        let (y_top, y_bottom) = luma_rows.split_at_mut(width);
        for ((((top, bottom), y_top), y_bottom), uv) in top
            .chunks_exact(8)
            .zip(bottom.chunks_exact(8))
            .zip(y_top.chunks_exact_mut(2))
            .zip(y_bottom.chunks_exact_mut(2))
            .zip(uv.chunks_exact_mut(2))
        {
            let pixels = [&top[..4], &top[4..], &bottom[..4], &bottom[4..]];
            y_top[0] = luma_of(pixels[0]);
            y_top[1] = luma_of(pixels[1]);
            y_bottom[0] = luma_of(pixels[2]);
            y_bottom[1] = luma_of(pixels[3]);
            let sum = |channel: usize| -> i32 {
                pixels
                    .iter()
                    .map(|pixel| i32::from(pixel[channel]))
                    .sum::<i32>()
                    / 4
            };
            let (r, g, b) = (sum(0), sum(1), sum(2));
            uv[0] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            uv[1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
}

#[inline(always)]
fn luma_of(pixel: &[u8]) -> u8 {
    let r = i32::from(pixel[0]);
    let g = i32::from(pixel[1]);
    let b = i32::from(pixel[2]);
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::rgba_to_nv12;

    /// The previous two-pass scalar conversion, kept as the bit-exact reference.
    fn reference(rgba: &[u8], width: usize, height: usize) -> Vec<u8> {
        let pixels = width * height;
        let mut output = vec![0; pixels + pixels / 2];
        for row in 0..height {
            for column in 0..width {
                let offset = (row * width + column) * 4;
                let r = i32::from(rgba[offset]);
                let g = i32::from(rgba[offset + 1]);
                let b = i32::from(rgba[offset + 2]);
                output[row * width + column] =
                    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
            }
        }
        for row in (0..height).step_by(2) {
            for column in (0..width).step_by(2) {
                let (mut r, mut g, mut b) = (0_i32, 0_i32, 0_i32);
                for y in 0..2 {
                    for x in 0..2 {
                        let offset = ((row + y) * width + column + x) * 4;
                        r += i32::from(rgba[offset]);
                        g += i32::from(rgba[offset + 1]);
                        b += i32::from(rgba[offset + 2]);
                    }
                }
                let (r, g, b) = (r / 4, g / 4, b / 4);
                let chroma = pixels + (row / 2) * width + column;
                output[chroma] =
                    (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                output[chroma + 1] =
                    (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
        output
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    #[test]
    fn converts_black_and_white() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut output = Vec::new();
        rgba_to_nv12(&rgba, 2, 2, &mut output).unwrap();
        assert_eq!(output, [16, 235, 16, 235, 128, 128]);
    }

    #[test]
    fn matches_the_scalar_reference_on_single_and_multi_threaded_sizes() {
        let mut output = vec![7; 3];
        for (width, height) in [(2, 2), (6, 4), (1_280, 720), (1_922, 1_082)] {
            let rgba = noise(width * height * 4);
            rgba_to_nv12(&rgba, width as u32, height as u32, &mut output).unwrap();
            assert_eq!(output, reference(&rgba, width, height), "{width}x{height}");
        }
    }

    #[test]
    fn rejects_odd_or_mismatched_frames() {
        let mut output = Vec::new();
        assert!(rgba_to_nv12(&[0; 12], 3, 1, &mut output).is_err());
        assert!(rgba_to_nv12(&[0; 12], 2, 2, &mut output).is_err());
    }

    #[test]
    #[ignore = "timing benchmark: cargo test --release nv12 -- --ignored --nocapture"]
    fn benchmark_4k() {
        let (width, height) = (3_840, 2_160);
        let rgba = noise(width * height * 4);
        let mut output = Vec::new();
        let started = std::time::Instant::now();
        for _ in 0..20 {
            std::hint::black_box(reference(&rgba, width, height));
        }
        let reference_ms = started.elapsed().as_secs_f64() * 1_000.0 / 20.0;
        let started = std::time::Instant::now();
        for _ in 0..20 {
            rgba_to_nv12(&rgba, width as u32, height as u32, &mut output).unwrap();
        }
        let fast_ms = started.elapsed().as_secs_f64() * 1_000.0 / 20.0;
        println!("4K RGBA->NV12: reference {reference_ms:.2} ms, fast {fast_ms:.2} ms");
    }
}
