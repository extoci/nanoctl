use crate::types::{MediaError, PixelFormat, RawFrame};

pub(crate) fn checked_dimensions(width: u32, height: u32) -> Result<usize, MediaError> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(MediaError::Encode(
            "NV12 dimensions must be non-zero and even".to_string(),
        ));
    }
    width
        .checked_mul(height)
        .map(|pixels| pixels as usize)
        .filter(|pixels| pixels.checked_add(pixels / 2).is_some())
        .ok_or_else(|| MediaError::Encode("frame dimensions overflow".to_string()))
}

/// Converts or copies a raw frame into tightly packed NV12 in `output`.
///
/// The output buffer is resized exactly; every byte is rewritten before success is returned.
pub fn frame_to_nv12(frame: &RawFrame, output: &mut Vec<u8>) -> Result<(), MediaError> {
    let pixels = checked_dimensions(frame.width, frame.height)?;
    output.resize(pixels + pixels / 2, 0);
    frame_to_nv12_slice(frame, output)
}

/// Converts or copies a raw frame into an exactly sized caller-provided NV12 buffer.
pub fn frame_to_nv12_slice(frame: &RawFrame, output: &mut [u8]) -> Result<(), MediaError> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let pixels = checked_dimensions(frame.width, frame.height)?;
    let nv12_len = pixels + pixels / 2;
    if output.len() != nv12_len {
        return Err(MediaError::Encode(
            "NV12 output length does not match its dimensions".to_string(),
        ));
    }

    match frame.format {
        PixelFormat::Nv12 => {
            if frame.pixels.len() != nv12_len {
                return Err(MediaError::Encode(
                    "NV12 frame length does not match its dimensions".to_string(),
                ));
            }
            output.copy_from_slice(&frame.pixels);
            Ok(())
        }
        PixelFormat::I420 => copy_i420(frame, width, height, pixels, output),
        PixelFormat::Rgba32 | PixelFormat::Bgra32 => convert_rgba(frame, width, pixels, output),
    }
}

fn copy_i420(
    frame: &RawFrame,
    width: usize,
    height: usize,
    pixels: usize,
    output: &mut [u8],
) -> Result<(), MediaError> {
    if frame.pixels.len() != pixels + pixels / 2 {
        return Err(MediaError::Encode(
            "I420 frame length does not match its dimensions".to_string(),
        ));
    }
    output[..pixels].copy_from_slice(&frame.pixels[..pixels]);
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let u_plane = pixels;
    let v_plane = pixels + chroma_width * chroma_height;
    for row in 0..chroma_height {
        for column in 0..chroma_width {
            let source = row * chroma_width + column;
            let target = pixels + row * width + column * 2;
            output[target] = frame.pixels[u_plane + source];
            output[target + 1] = frame.pixels[v_plane + source];
        }
    }
    Ok(())
}

fn convert_rgba(
    frame: &RawFrame,
    width: usize,
    pixels: usize,
    output: &mut [u8],
) -> Result<(), MediaError> {
    if frame.pixels.len() != pixels * 4 {
        return Err(MediaError::Encode(
            "RGBA/BGRA frame length does not match its dimensions".to_string(),
        ));
    }
    let (luma, chroma) = output.split_at_mut(pixels);
    let mut planar = yuv::YuvBiPlanarImageMut {
        y_plane: yuv::BufferStoreMut::Borrowed(luma),
        y_stride: frame.width,
        uv_plane: yuv::BufferStoreMut::Borrowed(chroma),
        uv_stride: frame.width,
        width: frame.width,
        height: frame.height,
    };
    let stride = (width * 4) as u32;
    let result = match frame.format {
        PixelFormat::Rgba32 => yuv::rgba_to_yuv_nv12(
            &mut planar,
            &frame.pixels,
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt601,
            yuv::YuvConversionMode::Balanced,
        ),
        PixelFormat::Bgra32 => yuv::bgra_to_yuv_nv12(
            &mut planar,
            &frame.pixels,
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt601,
            yuv::YuvConversionMode::Balanced,
        ),
        _ => unreachable!(),
    };
    result.map_err(|error| {
        MediaError::Encode(format!("pixel input to NV12 conversion failed: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::frame_to_nv12;
    use crate::types::{PixelFormat, RawFrame};

    fn frame(format: PixelFormat, width: u32, height: u32, pixels: Vec<u8>) -> RawFrame {
        RawFrame::new(0, width, height, format, pixels)
    }

    #[test]
    fn converts_black_and_white_rgba() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut output = Vec::new();
        frame_to_nv12(
            &frame(PixelFormat::Rgba32, 2, 2, rgba.to_vec()),
            &mut output,
        )
        .unwrap();
        assert_eq!(output, [16, 235, 16, 235, 128, 128]);
    }

    #[test]
    fn bgra_swizzles_channels() {
        // Red in BGRA byte order. Black/white cannot catch channel-order regressions.
        let bgra = [
            0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        let rgba = [
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let mut output = Vec::new();
        let mut expected = Vec::new();
        frame_to_nv12(
            &frame(PixelFormat::Bgra32, 2, 2, bgra.to_vec()),
            &mut output,
        )
        .unwrap();
        frame_to_nv12(
            &frame(PixelFormat::Rgba32, 2, 2, rgba.to_vec()),
            &mut expected,
        )
        .unwrap();
        assert_eq!(output, expected);
    }

    #[test]
    fn copies_nv12_without_rewriting() {
        let nv12 = vec![1, 2, 3, 4, 5, 6];
        let mut output = Vec::new();
        frame_to_nv12(&frame(PixelFormat::Nv12, 2, 2, nv12.clone()), &mut output).unwrap();
        assert_eq!(output, nv12);
    }

    #[test]
    fn interleaves_i420_chroma() {
        let i420 = vec![10, 20, 30, 40, 101, 201];
        let mut output = Vec::new();
        frame_to_nv12(&frame(PixelFormat::I420, 2, 2, i420), &mut output).unwrap();
        assert_eq!(output, [10, 20, 30, 40, 101, 201]);
    }

    #[test]
    fn rejects_invalid_input() {
        let mut output = Vec::new();
        assert!(
            frame_to_nv12(&frame(PixelFormat::Rgba32, 3, 2, vec![0; 24]), &mut output).is_err()
        );
        assert!(
            frame_to_nv12(&frame(PixelFormat::Rgba32, 2, 2, vec![0; 15]), &mut output).is_err()
        );
        assert!(frame_to_nv12(&frame(PixelFormat::Nv12, 2, 2, vec![0; 5]), &mut output).is_err());
    }
}
