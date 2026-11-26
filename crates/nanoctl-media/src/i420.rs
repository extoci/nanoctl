use crate::nv12::checked_dimensions;
use crate::types::{MediaError, PixelFormat, RawFrame};

/// Converts or copies a raw frame into tightly packed I420 in `output`.
///
/// The output layout is Y followed by U and V planes, all tightly packed. Every byte is
/// rewritten before success is returned.
pub fn frame_to_i420(frame: &RawFrame, output: &mut Vec<u8>) -> Result<(), MediaError> {
    let width = frame.width as usize;
    let pixels = checked_dimensions(frame.width, frame.height)?;
    let len = pixels + pixels / 2;
    output.resize(len, 0);

    match frame.format {
        PixelFormat::I420 => {
            if frame.pixels.len() != len {
                return Err(MediaError::Encode(
                    "I420 frame length does not match its dimensions".to_string(),
                ));
            }
            output.copy_from_slice(&frame.pixels);
            Ok(())
        }
        PixelFormat::Nv12 => copy_nv12(frame, pixels, output),
        PixelFormat::Rgba32 | PixelFormat::Bgra32 => convert_rgba(frame, width, pixels, output),
    }
}

fn copy_nv12(frame: &RawFrame, pixels: usize, output: &mut [u8]) -> Result<(), MediaError> {
    if frame.pixels.len() != pixels + pixels / 2 {
        return Err(MediaError::Encode(
            "NV12 frame length does not match its dimensions".to_string(),
        ));
    }
    output[..pixels].copy_from_slice(&frame.pixels[..pixels]);
    let chroma_len = pixels / 4;
    let (u_plane, v_plane) = output[pixels..].split_at_mut(chroma_len);
    for index in 0..chroma_len {
        u_plane[index] = frame.pixels[pixels + index * 2];
        v_plane[index] = frame.pixels[pixels + index * 2 + 1];
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
    let (y_plane, chroma) = output.split_at_mut(pixels);
    let (u_plane, v_plane) = chroma.split_at_mut(pixels / 4);
    let mut planar = yuv::YuvPlanarImageMut {
        y_plane: yuv::BufferStoreMut::Borrowed(y_plane),
        y_stride: frame.width,
        u_plane: yuv::BufferStoreMut::Borrowed(u_plane),
        u_stride: frame.width / 2,
        v_plane: yuv::BufferStoreMut::Borrowed(v_plane),
        v_stride: frame.width / 2,
        width: frame.width,
        height: frame.height,
    };
    let stride = (width * 4) as u32;
    let result = match frame.format {
        PixelFormat::Rgba32 => yuv::rgba_to_yuv420(
            &mut planar,
            &frame.pixels,
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt601,
            yuv::YuvConversionMode::Balanced,
        ),
        PixelFormat::Bgra32 => yuv::bgra_to_yuv420(
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
        MediaError::Encode(format!("pixel input to I420 conversion failed: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::frame_to_i420;
    use crate::types::{PixelFormat, RawFrame};

    fn frame(format: PixelFormat, width: u32, height: u32, pixels: Vec<u8>) -> RawFrame {
        RawFrame::new(0, width, height, format, pixels)
    }

    #[test]
    fn copies_i420_without_reordering() {
        let i420 = vec![1, 2, 3, 4, 101, 201];
        let mut output = Vec::new();
        frame_to_i420(&frame(PixelFormat::I420, 2, 2, i420.clone()), &mut output).unwrap();
        assert_eq!(output, i420);
    }

    #[test]
    fn deinterleaves_nv12_chroma() {
        let nv12 = vec![
            1, 2, 3, 4, 5, 6, 7, 8, // Y
            101, 201, 102, 202, // interleaved U/V
        ];
        let mut output = Vec::new();
        frame_to_i420(&frame(PixelFormat::Nv12, 4, 2, nv12), &mut output).unwrap();
        assert_eq!(output, [1, 2, 3, 4, 5, 6, 7, 8, 101, 102, 201, 202]);
    }

    #[test]
    fn converts_black_and_white_rgba() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut output = Vec::new();
        frame_to_i420(
            &frame(PixelFormat::Rgba32, 2, 2, rgba.to_vec()),
            &mut output,
        )
        .unwrap();
        assert_eq!(output, [16, 235, 16, 235, 128, 128]);
    }

    #[test]
    fn bgra_input_matches_swizzled_rgba() {
        let bgra = [
            0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        let rgba = [
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let mut output = Vec::new();
        let mut expected = Vec::new();
        frame_to_i420(
            &frame(PixelFormat::Bgra32, 2, 2, bgra.to_vec()),
            &mut output,
        )
        .unwrap();
        frame_to_i420(
            &frame(PixelFormat::Rgba32, 2, 2, rgba.to_vec()),
            &mut expected,
        )
        .unwrap();
        assert_eq!(output, expected);
    }

    #[test]
    fn rejects_invalid_input() {
        let mut output = Vec::new();
        assert!(
            frame_to_i420(&frame(PixelFormat::Rgba32, 3, 2, vec![0; 24]), &mut output).is_err()
        );
        assert!(frame_to_i420(&frame(PixelFormat::Nv12, 2, 2, vec![0; 5]), &mut output).is_err());
    }
}
