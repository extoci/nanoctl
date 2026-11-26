use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_SINGLETHREADED,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIDevice, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::Interface;
use xcap::Monitor;

use crate::types::{MediaError, PixelFormat, RawFrame, VideoSource};

#[derive(Debug, Clone, Copy)]
enum DisplaySelection {
    Primary,
    Id(u32),
}

#[derive(Clone)]
struct CapturedFrame {
    width: u32,
    height: u32,
    changed: bool,
    pixels: Arc<[u8]>,
}

/// Windows Desktop Duplication capture backend.
///
/// DXGI duplication objects are COM owners and must not move across arbitrary Tokio workers.
/// They are created, used, and dropped on one dedicated capture thread. The asynchronous
/// `VideoSource` is only a bounded request/response proxy. Capture is change-driven: an
/// unchanged desktop does not manufacture duplicate frames for the encoder.
pub struct DxgiCaptureSource {
    request_tx: Option<mpsc::SyncSender<u64>>,
    response_rx: tokio::sync::mpsc::Receiver<(u64, Result<CapturedFrame, String>)>,
    thread: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    next_ticket: u64,
    started: Instant,
}

impl DxgiCaptureSource {
    pub fn primary() -> Result<Self, MediaError> {
        Self::start(DisplaySelection::Primary)
    }

    pub fn display(display_id: u32) -> Result<Self, MediaError> {
        Self::start(DisplaySelection::Id(display_id))
    }

    fn start(selection: DisplaySelection) -> Result<Self, MediaError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, u32), String>>();
        let (request_tx, request_rx) = mpsc::sync_channel::<u64>(1);
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let thread = std::thread::spawn(move || {
            capture_thread(
                selection,
                ready_tx,
                request_rx,
                response_tx,
                worker_shutdown,
            );
        });
        ready_rx
            .recv()
            .map_err(|_| {
                MediaError::Capture("DXGI capture worker exited before startup".to_string())
            })?
            .map_err(MediaError::Capture)?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            thread: Some(thread),
            shutdown,
            next_ticket: 0,
            started: Instant::now(),
        })
    }
}

impl Drop for DxgiCaptureSource {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // A cancelled capture can leave a stale response queued. Closing the receiver makes a
        // worker blocked in `blocking_send` fail instead of deadlocking the join below.
        self.response_rx.close();
        self.request_tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[async_trait]
impl VideoSource for DxgiCaptureSource {
    async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError> {
        let Some(request_tx) = &self.request_tx else {
            return Ok(None);
        };
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        if request_tx.try_send(ticket).is_err() {
            return Err(MediaError::Capture(
                "DXGI capture worker is busy or stopped".to_string(),
            ));
        }
        loop {
            let Some((response_ticket, response)) = self.response_rx.recv().await else {
                return Ok(None);
            };
            if response_ticket != ticket {
                continue;
            }
            let frame = response.map_err(MediaError::Capture)?;
            let capture_time_us = self.started.elapsed().as_micros() as u64;
            let raw = if frame.changed {
                RawFrame::from_shared(
                    capture_time_us,
                    frame.width,
                    frame.height,
                    PixelFormat::Bgra32,
                    frame.pixels,
                )
            } else {
                RawFrame::unchanged_shared(
                    capture_time_us,
                    frame.width,
                    frame.height,
                    PixelFormat::Bgra32,
                    frame.pixels,
                )
            };
            return Ok(Some(raw));
        }
    }
}

fn capture_thread(
    selection: DisplaySelection,
    ready_tx: mpsc::Sender<Result<(u32, u32), String>>,
    request_rx: mpsc::Receiver<u64>,
    response_tx: tokio::sync::mpsc::Sender<(u64, Result<CapturedFrame, String>)>,
    shutdown: Arc<AtomicBool>,
) {
    let h_monitor = match select_hmonitor(selection) {
        Ok(h_monitor) => h_monitor,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let mut duplication = match Duplication::new(h_monitor) {
        Ok(duplication) => Some(duplication),
        Err(error) => {
            let _ = ready_tx.send(Err(error.to_string()));
            return;
        }
    };
    let initial = duplication.as_ref().expect("duplication present");
    if ready_tx.send(Ok((initial.width, initial.height))).is_err() {
        return;
    }

    let mut last_frame: Option<CapturedFrame> = None;
    while let Ok(ticket) = request_rx.recv() {
        loop {
            if shutdown.load(Ordering::Acquire) {
                return;
            }
            let result = duplication
                .as_mut()
                .expect("duplication present")
                .capture_changed(4);
            match result {
                Ok(Some(frame)) => {
                    last_frame = Some(frame.clone());
                    if response_tx.blocking_send((ticket, Ok(frame))).is_err() {
                        return;
                    }
                    break;
                }
                Ok(None) => {
                    if let Some(frame) = &last_frame {
                        let mut unchanged = frame.clone();
                        unchanged.changed = false;
                        if response_tx.blocking_send((ticket, Ok(unchanged))).is_err() {
                            return;
                        }
                        break;
                    }
                    continue;
                }
                Err(error) => {
                    // Desktop duplication sessions are invalidated by mode changes, display
                    // power events, and some secure-desktop transitions. Recreate once rather
                    // than treating every access loss as fatal.
                    drop(duplication.take());
                    match Duplication::new(h_monitor) {
                        Ok(recreated) => {
                            duplication = Some(recreated);
                            match duplication
                                .as_mut()
                                .expect("recreated duplication present")
                                .capture_changed(250)
                            {
                                Ok(Some(frame)) => {
                                    last_frame = Some(frame.clone());
                                    if response_tx.blocking_send((ticket, Ok(frame))).is_err() {
                                        return;
                                    }
                                    break;
                                }
                                Ok(None) => continue,
                                Err(second) => {
                                    let _ = response_tx
                                        .blocking_send((ticket, Err(second.to_string())));
                                    break;
                                }
                            }
                        }
                        Err(create_error) => {
                            let _ = response_tx.blocking_send((
                                ticket,
                                Err(format!("{error}; recreation failed: {create_error}")),
                            ));
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn select_hmonitor(selection: DisplaySelection) -> Result<HMONITOR, String> {
    let monitors = Monitor::all().map_err(|error| error.to_string())?;
    let monitor = match selection {
        DisplaySelection::Primary => monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .cloned()
            .ok_or_else(|| "no display is available".to_string())?,
        DisplaySelection::Id(display_id) => monitors
            .into_iter()
            .find(|monitor| monitor.id().is_ok_and(|id| id == display_id))
            .ok_or_else(|| "selected display is unavailable".to_string())?,
    };
    let id = monitor.id().map_err(|error| error.to_string())?;
    // xcap's Windows monitor id is the raw HMONITOR value encoded as u32.
    Ok(HMONITOR(id as usize as *mut _))
}

static ACTIVE_OUTPUTS: OnceLock<(Mutex<HashSet<usize>>, Condvar)> = OnceLock::new();

fn active_outputs() -> &'static (Mutex<HashSet<usize>>, Condvar) {
    ACTIVE_OUTPUTS.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

fn lock_active_outputs() -> MutexGuard<'static, HashSet<usize>> {
    active_outputs()
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Process-local exclusive ownership of one output duplication session. Windows rejects a
/// second `DuplicateOutput` for the same output, so racing capture instances must not turn
/// that platform rule into an opaque startup failure.
struct OutputPermit(usize);

impl OutputPermit {
    fn acquire(h_monitor: HMONITOR) -> Result<Self> {
        let key = h_monitor.0 as usize;
        let started = Instant::now();
        let mut active = lock_active_outputs();
        loop {
            if active.insert(key) {
                return Ok(Self(key));
            }
            let remaining = Duration::from_secs(3).saturating_sub(started.elapsed());
            if remaining.is_zero() {
                bail!("timed out waiting for the active DXGI output duplication");
            }
            let (guard, _) = active_outputs()
                .1
                .wait_timeout(active, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            active = guard;
        }
    }
}

impl Drop for OutputPermit {
    fn drop(&mut self) {
        lock_active_outputs().remove(&self.0);
        active_outputs().1.notify_one();
    }
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    _permit: OutputPermit,
    width: u32,
    height: u32,
}

impl Duplication {
    fn new(h_monitor: HMONITOR) -> Result<Self> {
        let permit = OutputPermit::acquire(h_monitor)?;
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
                if output.GetDesc()?.Monitor == h_monitor {
                    let output1 = output.cast::<IDXGIOutput1>()?;
                    let duplication = output1
                        .DuplicateOutput(&device)
                        .context("DuplicateOutput failed")?;
                    let desc = output.GetDesc()?;
                    let width =
                        (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left) as u32;
                    let height =
                        (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top) as u32;
                    let staging = create_staging(&device, width, height)?;
                    return Ok(Self {
                        device,
                        context,
                        duplication,
                        staging,
                        _permit: permit,
                        width,
                        height,
                    });
                }
            }
        }
    }

    /// Waits for a compositor update and returns one complete current BGRA frame. `None` is
    /// a timeout and means the desktop did not change; it is not end-of-stream.
    fn capture_changed(&mut self, timeout_ms: u32) -> Result<Option<CapturedFrame>> {
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
        let result = (|| -> Result<Option<CapturedFrame>> {
            if info.LastPresentTime == 0 {
                return Ok(None);
            }
            let texture = resource
                .as_ref()
                .and_then(|resource| resource.cast::<ID3D11Texture2D>().ok())
                .context("acquired DXGI resource is not a texture")?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };
            if desc.Width != self.width || desc.Height != self.height {
                self.width = desc.Width;
                self.height = desc.Height;
                self.staging = create_staging(&self.device, desc.Width, desc.Height)?;
            }
            let frame_len = (self.width as usize)
                .checked_mul(self.height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .context("DXGI frame dimensions overflow")?;
            let mut pixels = vec![0; frame_len];
            self.copy_full(&texture, &mut pixels)?;
            Ok(Some(CapturedFrame {
                width: self.width,
                height: self.height,
                changed: true,
                pixels: pixels.into(),
            }))
        })();
        unsafe {
            let _ = self.duplication.ReleaseFrame();
        }
        result
    }

    fn copy_full(&mut self, texture: &ID3D11Texture2D, output: &mut [u8]) -> Result<()> {
        unsafe {
            self.context
                .CopySubresourceRegion(&self.staging, 0, 0, 0, 0, texture, 0, None);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let copy_result = (|| -> Result<()> {
                let src = mapped.pData as *const u8;
                if src.is_null() {
                    bail!("DXGI staging texture mapped to a null pointer");
                }
                let row_pitch = mapped.RowPitch as usize;
                let width = self.width as usize;
                let row_bytes = width * 4;
                for y in 0..self.height as usize {
                    let src_row = std::slice::from_raw_parts(src.add(y * row_pitch), row_bytes);
                    let start = y * row_bytes;
                    output[start..start + row_bytes].copy_from_slice(src_row);
                }
                Ok(())
            })();
            self.context.Unmap(&self.staging, 0);
            copy_result
        }
    }
}

fn create_device() -> Result<ID3D11Device> {
    unsafe {
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            let mut device = None;
            if D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .is_ok()
                && let Some(device) = device
            {
                return Ok(device);
            }
        }
        bail!("D3D11CreateDevice failed for hardware and WARP drivers")
    }
}

fn create_staging(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
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
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };
    staging.context("DXGI staging texture creation failed")
}

#[cfg(test)]
mod tests {
    use super::DxgiCaptureSource;
    use crate::types::VideoSource;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[ignore = "requires a real interactive Windows display"]
    async fn captures_primary_display_shape() {
        let mut source = DxgiCaptureSource::primary().unwrap();
        let frame = source.next_frame().await.unwrap().unwrap();
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(
            frame.pixels.len(),
            frame.width as usize * frame.height as usize * 4
        );
        let second = tokio::time::timeout(std::time::Duration::from_secs(1), source.next_frame())
            .await
            .expect("unchanged-frame reuse timed out")
            .unwrap()
            .unwrap();
        assert_eq!(second.width, frame.width);
        assert_eq!(second.height, frame.height);
    }
}
