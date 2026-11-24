//! Windows DXGI Desktop Duplication capture.
//!
//! Unlike the xcap `VideoRecorder` fallback this only emits a frame when the
//! compositor reports changed pixels, reads back the union of the frame's
//! dirty/move rects instead of the whole texture, and tags the frame as
//! backend-verified changed so the producer can skip its full-frame memcmp.

use std::sync::{Arc, Weak, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::Interface;
use xcap::{Frame, Monitor};

use super::{LatestFrame, PipelineStats};

struct Duplication {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    /// Latest full RGBA image; dirty rects are patched into it between emits.
    accum: Vec<u8>,
}

fn create_device() -> Result<ID3D11Device> {
    unsafe {
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let mut device = None;
            if let Ok(()) = D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            ) && let Some(device) = device
            {
                return Ok(device);
            }
        }
        bail!("D3D11CreateDevice failed for hardware and WARP drivers")
    }
}

impl Duplication {
    fn new(h_monitor: HMONITOR) -> Result<Self> {
        unsafe {
            let device = create_device()?;
            let dxgi_device = device.cast::<IDXGIDevice>()?;
            let context = device.GetImmediateContext()?;
            let adapter = dxgi_device.GetAdapter()?;
            let mut index = 0;
            loop {
                let output = adapter
                    .EnumOutputs(index)
                    .context("DXGI output not found for monitor")?;
                index += 1;
                let output1 = output.cast::<IDXGIOutput1>()?;
                let duplication = output1
                    .DuplicateOutput(&device)
                    .context("DuplicateOutput failed")?;
                if output.GetDesc()?.Monitor == h_monitor {
                    let desc = output.GetDesc()?;
                    let width =
                        (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
                    let height =
                        (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;
                    let staging_desc = D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        MiscFlags: 0,
                    };
                    let mut staging = None;
                    device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
                    let staging = staging.context("staging texture creation failed")?;
                    return Ok(Self {
                        context,
                        duplication,
                        staging,
                        width,
                        height,
                        accum: vec![0; (width * height * 4) as usize],
                    });
                }
            }
        }
    }

    /// Waits up to `timeout_ms` for a compositor frame; when one arrives and it
    /// carries changed pixels, patches the dirty regions into `accum` and
    /// returns a full RGBA frame. Returns `None` on timeout or metadata-only
    /// frames (cursor moves without pixel changes).
    fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        unsafe {
            if let Err(error) =
                self.duplication
                    .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
            {
                let _ = self.duplication.ReleaseFrame();
                if error.code() == DXGI_ERROR_WAIT_TIMEOUT {
                    return Ok(None);
                }
                return Err(error.into());
            }
        }
        let result = (|| -> Result<Option<Frame>> {
            if info.LastPresentTime == 0 {
                return Ok(None);
            }
            let mut rects = self.frame_rects()?;
            rects.extend(self.frame_move_rects()?);
            if rects.is_empty() {
                return Ok(None);
            }
            let texture = resource
                .as_ref()
                .and_then(|r| r.cast::<ID3D11Texture2D>().ok())
                .context("acquired resource is not a texture")?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };
            if desc.Width != self.width || desc.Height != self.height {
                self.width = desc.Width;
                self.height = desc.Height;
                self.accum = vec![0; (desc.Width * desc.Height * 4) as usize];
                // Mode change: treat the whole frame as dirty.
                rects = vec![RECT {
                    left: 0,
                    top: 0,
                    right: desc.Width as i32,
                    bottom: desc.Height as i32,
                }];
            }
            self.copy_dirty(&texture, &rects)?;
            Ok(Some(Frame::new(
                self.width,
                self.height,
                self.accum.clone(),
            )))
        })();
        unsafe {
            let _ = self.duplication.ReleaseFrame();
        }
        result
    }

    fn frame_rects(&self) -> Result<Vec<RECT>> {
        unsafe {
            let mut rects = vec![RECT::default(); 256];
            loop {
                let size = (rects.len() * size_of::<RECT>()) as u32;
                let mut needed = 0u32;
                match self
                    .duplication
                    .GetFrameDirtyRects(size, rects.as_mut_ptr(), &mut needed)
                {
                    Ok(()) => {
                        rects.truncate(needed as usize / size_of::<RECT>());
                        return Ok(rects);
                    }
                    Err(error) if needed as usize / size_of::<RECT>() > rects.len() => {
                        rects.resize(needed as usize / size_of::<RECT>(), RECT::default());
                        if rects.len() > 8192 {
                            return Err(error.into());
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    fn frame_move_rects(&self) -> Result<Vec<RECT>> {
        unsafe {
            let mut moves = vec![DXGI_OUTDUPL_MOVE_RECT::default(); 256];
            loop {
                let size = (moves.len() * size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32;
                let mut needed = 0u32;
                match self
                    .duplication
                    .GetFrameMoveRects(size, moves.as_mut_ptr(), &mut needed)
                {
                    Ok(()) => {
                        return Ok(moves
                            .iter()
                            .take(needed as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>())
                            .map(|m| m.DestinationRect)
                            .collect());
                    }
                    Err(error)
                        if needed as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>() > moves.len() =>
                    {
                        moves.resize(
                            needed as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>(),
                            DXGI_OUTDUPL_MOVE_RECT::default(),
                        );
                        if moves.len() > 8192 {
                            return Err(error.into());
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    /// Copies each dirty rect GPU→staging (sub-resource copies, so only dirty
    /// data crosses the bus), then swizzles BGRA→RGBA into `accum`.
    fn copy_dirty(&mut self, texture: &ID3D11Texture2D, rects: &[RECT]) -> Result<()> {
        unsafe {
            for rect in rects {
                if rect.right <= rect.left || rect.bottom <= rect.top {
                    continue;
                }
                let region = D3D11_BOX {
                    left: rect.left as u32,
                    top: rect.top as u32,
                    right: rect.right as u32,
                    bottom: rect.bottom as u32,
                    front: 0,
                    back: 1,
                };
                self.context.CopySubresourceRegion(
                    &self.staging,
                    0,
                    rect.left as u32,
                    rect.top as u32,
                    0,
                    texture,
                    0,
                    Some(&region),
                );
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let src = mapped.pData as *const u8;
            let row_pitch = mapped.RowPitch as usize;
            for rect in rects {
                let left = (rect.left.max(0) as usize).min(self.width as usize);
                let right = (rect.right as usize).min(self.width as usize);
                let top = (rect.top.max(0) as usize).min(self.height as usize);
                let bottom = (rect.bottom as usize).min(self.height as usize);
                for y in top..bottom {
                    let src_row = src.add(y * row_pitch + left * 4);
                    let dst_start = (y * self.width as usize + left) * 4;
                    let dst_row = &mut self.accum[dst_start..dst_start + (right - left) * 4];
                    for x in 0..(right - left) {
                        let b = *src_row.add(x * 4);
                        let g = *src_row.add(x * 4 + 1);
                        let r = *src_row.add(x * 4 + 2);
                        let a = *src_row.add(x * 4 + 3);
                        dst_row[x * 4] = r;
                        dst_row[x * 4 + 1] = g;
                        dst_row[x * 4 + 2] = b;
                        dst_row[x * 4 + 3] = a;
                    }
                }
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(())
    }
}

/// Starts a DXGI duplication capture loop for `monitor`. Returns the latest
/// frame slot and the owning thread; mirrors `linux_damage::start`. On failure
/// the caller falls back to the xcap recorder path.
pub(super) fn start(
    monitor: &Monitor,
    stats: Arc<PipelineStats>,
) -> Result<(Arc<LatestFrame>, JoinHandle<()>)> {
    // xcap encodes the raw HMONITOR pointer in `id()`.
    let h_monitor_raw = monitor.id()? as usize;
    let h_monitor = HMONITOR(h_monitor_raw as *mut _);
    // Prove the duplication can be created on this thread before spawning —
    // failure here takes the xcap fallback instead of a dead capture thread.
    let dup = Duplication::new(h_monitor)?;
    let latest = Arc::new(LatestFrame {
        stats,
        ..Default::default()
    });
    let slot = Arc::downgrade(&latest);
    let thread = std::thread::Builder::new()
        .name("nanoctl-dxgi-capture".to_owned())
        .spawn(move || {
            run(h_monitor_raw, dup, slot.clone());
            if let Some(latest) = slot.upgrade() {
                latest.finished.store(true, Ordering::Release);
                latest.ready.notify_one();
            }
        })
        .context("DXGI capture thread could not start")?;
    Ok((latest, thread))
}

fn run(h_monitor_raw: usize, mut dup: Duplication, slot: Weak<LatestFrame>) {
    let h_monitor = HMONITOR(h_monitor_raw as *mut _);
    loop {
        let Some(latest) = slot.upgrade() else {
            return;
        };
        if latest.stopped.load(Ordering::Acquire) {
            return;
        }
        match dup.acquire(100) {
            Ok(Some(frame)) => {
                // Backend-verified change: producer skips the memcmp.
                if !latest.replace_hint(frame, Some(true)) {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                let is_access_lost = error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|e| e.code() == DXGI_ERROR_ACCESS_LOST);
                if !is_access_lost {
                    tracing::warn!(%error, "DXGI duplication capture ended");
                    return;
                }
                // Desktop switch / mode change: rebuild the duplication.
                match Duplication::new(h_monitor) {
                    Ok(next) => dup = next,
                    Err(recreate_error) => {
                        tracing::warn!(%recreate_error, "DXGI duplication recreate failed");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        }
    }
}
