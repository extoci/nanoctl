use anyhow::{Context, Result, ensure};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer,
    images::{Image, ImageRef},
};
use xcap::image::RgbaImage;

#[derive(Default)]
pub(super) struct RgbaResizer {
    resizer: Resizer,
}

impl RgbaResizer {
    pub(super) fn resize(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
        target_width: u32,
        target_height: u32,
    ) -> Result<RgbaImage> {
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
        let source = ImageRef::new(width, height, pixels, PixelType::U8x4)?;
        let mut output = Image::new(target_width, target_height, PixelType::U8x4);
        self.resizer.resize(
            &source,
            &mut output,
            &ResizeOptions::new()
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                .use_alpha(false),
        )?;
        RgbaImage::from_raw(target_width, target_height, output.into_vec())
            .context("resizer returned invalid RGBA pixels")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcap::image::{Rgba, imageops};

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
            assert_eq!(actual.dimensions(), expected.dimensions());
            for (actual, expected) in actual.as_raw().iter().zip(expected.as_raw()) {
                assert!(actual.abs_diff(*expected) <= 2, "{actual} != {expected}");
            }
        }
    }

    #[test]
    fn triangle_preserves_constant_rgba_without_premultiplication() {
        let source = RgbaImage::from_pixel(80, 60, Rgba([23, 42, 199, 17]));
        let output = RgbaResizer::default()
            .resize(source.as_raw(), 80, 60, 48, 36)
            .unwrap();
        assert!(output.pixels().all(|pixel| pixel.0 == [23, 42, 199, 17]));
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
