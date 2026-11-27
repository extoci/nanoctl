//! Windows cursor shape and position capture.
//!
//! DXGI intentionally excludes the compositor cursor from the desktop image.
//! The viewer therefore receives the real cursor as metadata and composits it
//! locally, avoiding the encode cost of burning a moving cursor into video.

use anyhow::{Context, Result};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetObjectW, HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, CopyIcon, DI_NORMAL, DestroyCursor, DrawIconEx, GetCursorInfo,
    GetIconInfo, HCURSOR, HICON, ICONINFO, IDC_ARROW, LoadCursorW,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CursorShape {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) hotspot_x: u32,
    pub(crate) hotspot_y: u32,
    /// RGBA bytes, row-major.
    pub(crate) rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct CursorSample {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) visible: bool,
    pub(crate) shape_key: usize,
    pub(crate) shape: Option<CursorShape>,
}

pub(crate) fn sample(previous_shape_key: Option<usize>) -> Result<CursorSample> {
    unsafe {
        let mut info = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        GetCursorInfo(&mut info).context("GetCursorInfo failed")?;
        let shape_key = info.hCursor.0 as usize;
        let shape = (previous_shape_key != Some(shape_key))
            .then(|| {
                let captured = capture_shape(HICON(info.hCursor.0));
                captured.ok()
            })
            .flatten();
        Ok(CursorSample {
            x: info.ptScreenPos.x,
            y: info.ptScreenPos.y,
            visible: (info.flags.0 & CURSOR_SHOWING.0) != 0,
            shape_key,
            shape,
        })
    }
}

struct OwnedCursor(HICON);

impl Drop for OwnedCursor {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyCursor(HCURSOR(self.0.0));
        }
    }
}

fn bitmap_size(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<(i32, i32)> {
    if bitmap.is_invalid() {
        return None;
    }
    unsafe {
        let mut object = BITMAP::default();
        let read = GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut object as *mut BITMAP as *mut _),
        );
        (read == size_of::<BITMAP>() as i32).then_some((object.bmWidth, object.bmHeight))
    }
}

fn capture_shape(hcursor: HICON) -> Result<CursorShape> {
    unsafe {
        let copied = OwnedCursor(CopyIcon(hcursor).context("CopyIcon failed")?);
        let mut icon = ICONINFO::default();
        GetIconInfo(copied.0, &mut icon).context("GetIconInfo failed")?;
        let color_size = bitmap_size(icon.hbmColor);
        let mask_size = bitmap_size(icon.hbmMask);
        let (width, mut height) = color_size.or(mask_size).context("cursor bitmap missing")?;
        if color_size.is_none() {
            // Monochrome cursor masks store the AND plane above the XOR plane.
            height /= 2;
        }
        if width <= 0 || height <= 0 || width > 256 || height > 256 {
            anyhow::bail!("implausible cursor dimensions {width}x{height}");
        }

        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            anyhow::bail!("CreateCompatibleDC failed");
        }
        let result = (|| -> Result<CursorShape> {
            let bitmap_info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height requests a top-down DIB.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: (width * height * 4) as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
                    .context("CreateDIBSection failed")?;
            let result = (|| -> Result<CursorShape> {
                if bits.is_null() {
                    anyhow::bail!("CreateDIBSection returned no pixel memory");
                }
                let len = (width * height * 4) as usize;
                std::ptr::write_bytes(bits, 0, len);
                let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
                DrawIconEx(dc, 0, 0, copied.0, width, height, 0, None, DI_NORMAL)
                    .context("DrawIconEx failed")?;
                SelectObject(dc, previous);
                let bgra = std::slice::from_raw_parts(bits as *const u8, len);
                let mut rgba = Vec::with_capacity(len);
                for px in bgra.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                // Some legacy monochrome cursors do not write an alpha channel.
                // Keep their visible ink rather than emitting a fully transparent image.
                if rgba.chunks_exact(4).all(|px| px[3] == 0) {
                    for px in rgba.chunks_exact_mut(4) {
                        px[3] = if px[..3].iter().any(|channel| *channel != 0) {
                            255
                        } else {
                            0
                        };
                    }
                }
                Ok(CursorShape {
                    width: width as u32,
                    height: height as u32,
                    hotspot_x: icon.xHotspot,
                    hotspot_y: icon.yHotspot,
                    rgba,
                })
            })();
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            result
        })();
        let _ = DeleteDC(dc);
        if !icon.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon.hbmColor.0));
        }
        if !icon.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(icon.hbmMask.0));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows cursor"]
    fn sample_captures_current_cursor() {
        let sample = sample(None).expect("cursor sample");
        if sample.shape_key == 0 {
            return;
        }
        let shape = sample.shape.expect("cursor shape");
        assert!(shape.width > 0 && shape.height > 0);
        assert!(
            shape.rgba.chunks_exact(4).any(|px| px[3] != 0),
            "captured cursor is fully transparent"
        );
    }

    #[test]
    fn system_arrow_captures_visible_pixels() {
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW).expect("system arrow cursor") };
        let shape = capture_shape(HICON(cursor.0)).expect("capture system arrow");
        assert!(shape.width > 0 && shape.height > 0);
        let opaque = shape.rgba.chunks_exact(4).filter(|px| px[3] != 0).count();
        assert!(opaque > 20, "only {opaque} opaque cursor pixels");
    }
}
