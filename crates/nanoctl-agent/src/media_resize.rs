use anyhow::{Context, Result, ensure};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
#[derive(Default)]
pub(super) struct RgbaResizer {
    resizer: Resizer,
    output: Vec<u8>,
}

impl RgbaResizer {
    pub(super) fn resize(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<&[u8]> {
        ensure!(
            width > 0 && height > 0 && target_width > 0 && target_height > 0,
            "resize dimensions must be non-zero"
        );
        ensure!(
            (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                == Some(pixels.len()),
            "resize pixels do not match RGBA dimensions"
        );
        let output_len = (target_width as usize)
            .checked_mul(target_height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("resize output dimensions overflow")?;
        if self.output.len() != output_len {
            self.output.clear();
            self.output.resize(output_len, 0);
        }
        let source = ImageRef::new(width, height, pixels, PixelType::U8x4)?;
        {
            let mut output = Image::from_slice_u8(
                target_width,
                target_height,
                &mut self.output,
                PixelType::U8x4,
            )?;
            self.resizer.resize(
                &source,
                &mut output,
                &ResizeOptions::new()
                    .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                    .use_alpha(false),
            )?;
        }
        Ok(&self.output[..output_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::{Rgba, RgbaImage, imageops};

    #[test]
    fn triangle_matches_reference_across_dimensions_and_reuses_scratch() {
        let source = RgbaImage::from_fn(80, 60, |x, y| {
            Rgba([(x * 3) as u8, (y * 4) as u8, ((x + y) * 7) as u8, 255])
        });
        let mut resizer = RgbaResizer::default();
        for (width, height) in [(48, 36), (80, 60), (32, 24), (96, 72), (2, 2)] {
            let expected = imageops::resize(&source, width, height, imageops::FilterType::Triangle);
            let actual = resizer
                .resize(source.as_raw(), 80, 60, width, height)
                .unwrap();
            assert_eq!(actual.len(), expected.as_raw().len());
            for (actual, expected) in actual.iter().zip(expected.as_raw()) {
                assert!(actual.abs_diff(*expected) <= 2, "{actual} != {expected}");
            }
        }
    }

    #[test]
    fn repeated_same_size_resize_reuses_the_output_allocation() {
        let source = RgbaImage::from_pixel(80, 60, Rgba([23, 42, 199, 17]));
        let mut resizer = RgbaResizer::default();
        let first = resizer
            .resize(source.as_raw(), 80, 60, 48, 36)
            .unwrap()
            .as_ptr();
        for _ in 0..16 {
            let actual = resizer.resize(source.as_raw(), 80, 60, 48, 36).unwrap();
            assert_eq!(actual.as_ptr(), first);
            assert!(
                actual
                    .chunks_exact(4)
                    .all(|pixel| pixel == [23, 42, 199, 17])
            );
        }
    }

    #[test]
    fn shrinking_and_returning_within_capacity_reuses_the_output_allocation() {
        let source = RgbaImage::from_pixel(80, 60, Rgba([23, 42, 199, 17]));
        let mut resizer = RgbaResizer::default();
        let first = resizer
            .resize(source.as_raw(), 80, 60, 80, 60)
            .unwrap()
            .as_ptr();
        assert_eq!(
            resizer
                .resize(source.as_raw(), 80, 60, 32, 24)
                .unwrap()
                .as_ptr(),
            first
        );
        assert_eq!(
            resizer
                .resize(source.as_raw(), 80, 60, 64, 48)
                .unwrap()
                .as_ptr(),
            first
        );
    }

    #[test]
    fn triangle_rejects_empty_invalid_or_mismatched_buffers() {
        let mut resizer = RgbaResizer::default();
        assert!(resizer.resize(&[], 0, 0, 48, 36).is_err());
        assert!(resizer.resize(&[0; 16], 2, 2, 0, 0).is_err());
        assert!(resizer.resize(&[0; 15], 2, 2, 1, 1).is_err());
        assert!(resizer.resize(&[0; 17], 2, 2, 1, 1).is_err());
    }
}
