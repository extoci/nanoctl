//! BT.601 limited-range RGBA/BGRA to NV12 conversion for encoders that take system-memory NV12.
//! Uses the `yuv` crate's runtime-dispatched SIMD: one SIMD thread is both faster and
//! many core-seconds cheaper than the previous multi-threaded scalar conversion.

use anyhow::{Context, Result, bail};

use crate::pixel_format::PixelFormat;

#[cfg(test)]
pub fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32, output: &mut Vec<u8>) -> Result<()> {
    pixels_to_nv12(rgba, width, height, PixelFormat::Rgba, output)
}

#[cfg(test)]
pub fn pixels_to_nv12(
    rgba: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    output: &mut Vec<u8>,
) -> Result<()> {
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
    pixels_to_nv12_slice(rgba, width as u32, height as u32, format, output)
}

pub fn pixels_to_nv12_slice(
    rgba: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    output: &mut [u8],
) -> Result<()> {
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
    let nv12_len = pixels
        .checked_add(pixels / 2)
        .context("NV12 size overflows")?;
    if output.len() != nv12_len {
        bail!("NV12 output length does not match its dimensions");
    }
    let (luma, chroma) = output.split_at_mut(pixels);
    let mut planar = yuv::YuvBiPlanarImageMut {
        y_plane: yuv::BufferStoreMut::Borrowed(luma),
        y_stride: width as u32,
        uv_plane: yuv::BufferStoreMut::Borrowed(chroma),
        uv_stride: width as u32,
        width: width as u32,
        height: height as u32,
    };
    let stride = (width * 4) as u32;
    let result = match format {
        PixelFormat::Rgba => yuv::rgba_to_yuv_nv12(
            &mut planar,
            rgba,
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt601,
            yuv::YuvConversionMode::Balanced,
        ),
        PixelFormat::Bgra => yuv::bgra_to_yuv_nv12(
            &mut planar,
            rgba,
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt601,
            yuv::YuvConversionMode::Balanced,
        ),
    };
    result.map_err(|error| anyhow::anyhow!("pixel input to NV12 conversion failed: {error}"))?;
    Ok(())
}

#[cfg(test)]
fn convert_band(
    rgba: &[u8],
    luma: &mut [u8],
    chroma: &mut [u8],
    width: usize,
    format: PixelFormat,
) {
    match format {
        PixelFormat::Rgba => convert_band_impl::<false>(rgba, luma, chroma, width),
        PixelFormat::Bgra => convert_band_impl::<true>(rgba, luma, chroma, width),
    }
}

#[cfg(test)]
fn convert_band_impl<const BGRA: bool>(
    rgba: &[u8],
    luma: &mut [u8],
    chroma: &mut [u8],
    width: usize,
) {
    let (r, g, b) = if BGRA { (2, 1, 0) } else { (0, 1, 2) };

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
            y_top[0] = luma_of(pixels[0], r, g, b);
            y_top[1] = luma_of(pixels[1], r, g, b);
            y_bottom[0] = luma_of(pixels[2], r, g, b);
            y_bottom[1] = luma_of(pixels[3], r, g, b);
            let sum = |channel: usize| -> i32 {
                pixels
                    .iter()
                    .map(|pixel| i32::from(pixel[channel]))
                    .sum::<i32>()
                    / 4
            };
            let (r, g, b) = (sum(r), sum(g), sum(b));
            uv[0] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            uv[1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
}

#[inline(always)]
#[cfg(test)]
fn luma_of(pixel: &[u8], ri: usize, gi: usize, bi: usize) -> u8 {
    let r = i32::from(pixel[ri]);
    let g = i32::from(pixel[gi]);
    let b = i32::from(pixel[bi]);
    (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::{PixelFormat, convert_band, pixels_to_nv12, rgba_to_nv12};

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

    /// The previous multi-threaded scalar conversion, retained only as a benchmark baseline.
    fn parallel_scalar(rgba: &[u8], width: usize, height: usize, format: PixelFormat) -> Vec<u8> {
        let pixels = width * height;
        let mut output = vec![0; pixels + pixels / 2];
        let (luma, chroma) = output.split_at_mut(pixels);
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(8));
        let row_pairs_per_band = (height / 2).div_ceil(threads);
        let mut bands = rgba
            .chunks(row_pairs_per_band * 2 * width * 4)
            .zip(luma.chunks_mut(row_pairs_per_band * 2 * width))
            .zip(chroma.chunks_mut(row_pairs_per_band * width));
        let Some(((first_rgba, first_luma), first_chroma)) = bands.next() else {
            return output;
        };
        std::thread::scope(|scope| {
            for ((rgba, luma), chroma) in bands {
                scope.spawn(move || convert_band(rgba, luma, chroma, width, format));
            }
            convert_band(first_rgba, first_luma, first_chroma, width, format);
        });
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
            let expected = reference(&rgba, width, height);
            let max_diff = output
                .iter()
                .zip(&expected)
                .map(|(actual, expected)| actual.abs_diff(*expected))
                .max()
                .unwrap();
            assert!(max_diff <= 1, "{width}x{height} max_diff={max_diff}");
        }
    }

    #[test]
    fn bgra_input_matches_swizzled_rgba_reference() {
        for (width, height) in [(6, 4), (1_280, 720)] {
            let bgra = noise(width * height * 4);
            let mut rgba = bgra.clone();
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let mut from_bgra = Vec::new();
            let mut expected = Vec::new();
            pixels_to_nv12(
                &bgra,
                width as u32,
                height as u32,
                PixelFormat::Bgra,
                &mut from_bgra,
            )
            .unwrap();
            rgba_to_nv12(&rgba, width as u32, height as u32, &mut expected).unwrap();
            assert_eq!(from_bgra, expected, "{width}x{height}");
            let scalar = reference(&rgba, width, height);
            let max_diff = from_bgra
                .iter()
                .zip(&scalar)
                .map(|(actual, expected)| actual.abs_diff(*expected))
                .max()
                .unwrap();
            assert!(max_diff <= 1, "{width}x{height} max_diff={max_diff}");
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
            std::hint::black_box(parallel_scalar(&rgba, width, height, PixelFormat::Rgba));
        }
        let scalar_ms = started.elapsed().as_secs_f64() * 1_000.0 / 20.0;
        let started = std::time::Instant::now();
        for _ in 0..20 {
            rgba_to_nv12(&rgba, width as u32, height as u32, &mut output).unwrap();
        }
        let fast_ms = started.elapsed().as_secs_f64() * 1_000.0 / 20.0;
        println!(
            "4K RGBA->NV12: reference {reference_ms:.2} ms, scalar-parallel {scalar_ms:.2} ms, SIMD {fast_ms:.2} ms"
        );
    }
}
