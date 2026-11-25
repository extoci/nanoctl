//! Windows DXGI Desktop Duplication capture.
//!
//! Unlike the xcap `VideoRecorder` fallback this only emits a frame when the
//! compositor reports changed pixels, reads back the union of the frame's
//! dirty/move rects instead of the whole texture, and tags the frame as
//! backend-verified changed so the producer can skip its full-frame memcmp.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

/// Native BGRA bytes of one dirty rect.
type Patch = (RECT, Vec<u8>);
/// Dirty-region pixels of a contiguous run of small-dirty emits.
type PatchLog = std::collections::VecDeque<(u64, Vec<Patch>)>;

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

/// Exclusive process-local ownership of one output's duplication session.
/// Windows rejects a second `DuplicateOutput` while another session for the
/// same output exists; this includes nanoctl's replay warmup racing its real
/// capture pipeline. The permit is stored after `duplication` below so struct
/// drop order releases the COM session before another waiter may open one.
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

    fn try_acquire(h_monitor: HMONITOR) -> Result<Self> {
        let key = h_monitor.0 as usize;
        if lock_active_outputs().insert(key) {
            Ok(Self(key))
        } else {
            bail!("DXGI output duplication is already active")
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
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    _permit: OutputPermit,
    width: u32,
    height: u32,
    /// Latest full BGRA image; dirty rects are patched into it between emits.
    accum: Vec<u8>,
    /// Number of frames emitted so far — recycled buffers older than this
    /// generation need the logged patches below to catch up.
    emit_gen: u64,
    /// Pixel data of recent emits' dirty regions: (emit generation, per-rect
    /// (rect, native BGRA bytes)) for a contiguous run of small-dirty
    /// emits. A high-dirty emit clears it (storing patches would cost more
    /// than the clone it saves), so entries are a gap-free run.
    patch_log: PatchLog,
    /// First emit generation covered by `patch_log` (== next emit when empty).
    /// A recycled buffer emitted at gen >= log_base - 1 can be brought
    /// current by replaying the log.
    log_base: u64,
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
        Self::with_permit(h_monitor, OutputPermit::acquire(h_monitor)?)
    }

    fn try_new(h_monitor: HMONITOR) -> Result<Self> {
        Self::with_permit(h_monitor, OutputPermit::try_acquire(h_monitor)?)
    }

    fn with_permit(h_monitor: HMONITOR, permit: OutputPermit) -> Result<Self> {
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
                        _permit: permit,
                        width,
                        height,
                        accum: vec![0; (width * height * 4) as usize],
                        emit_gen: 0,
                        patch_log: PatchLog::new(),
                        log_base: 0,
                    });
                }
            }
        }
    }

    /// Waits up to `timeout_ms` for a compositor frame; when one arrives and it
    /// carries changed pixels, patches the dirty regions into `accum` and
    /// returns a full BGRA frame. Returns `None` on timeout or metadata-only
    /// frames (cursor moves without pixel changes).
    /// `latest` supplies recycled emit buffers: the frame this thread emitted
    /// last (emit_gen == self.emit_gen) comes back once every Arc is dropped,
    /// still holding the previous image — patching the current dirty rects
    /// makes it identical to `accum` without a full-frame clone.
    fn acquire(&mut self, timeout_ms: u32, latest: &LatestFrame) -> Result<Option<Frame>> {
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
            // A recycled buffer emitted at gen g already contains the image
            // through emit g; it can be brought current iff every emit in
            // (g, emit_gen] was logged — i.e. g + 1 >= log_base.
            let recycled = latest
                .take_recycled()
                .into_iter()
                .filter(|(_, buf)| buf.len() == self.accum.len())
                .filter(|(eg, _)| *eg + 1 >= self.log_base && *eg <= self.emit_gen)
                .max_by_key(|(eg, _)| *eg)
                .map(|(_, buf)| buf);
            let mut emit = if let Some(mut buf) = recycled {
                self.replay_patches(&mut buf);
                buf
            } else {
                self.accum.clone()
            };
            let dirty_pixels: u64 = rects
                .iter()
                .map(|r| {
                    (r.right.max(0) - r.left.max(0)).max(0) as u64
                        * (r.bottom.max(0) - r.top.max(0)).max(0) as u64
                })
                .sum();
            let log_patches = dirty_pixels * 4 < self.accum.len() as u64;
            let logged = self.copy_dirty(&texture, &rects, &mut emit, log_patches)?;
            self.emit_gen += 1;
            match logged {
                Some(patches) => {
                    self.patch_log.push_back((self.emit_gen, patches));
                    while self.patch_log.len() > 4 {
                        self.patch_log.pop_front();
                    }
                    self.log_base = self
                        .patch_log
                        .front()
                        .map(|(eg, _)| *eg)
                        .unwrap_or(self.emit_gen + 1);
                }
                None => {
                    // A large-dirty emit breaks the logged run: buffers older
                    // than this emit can no longer catch up.
                    self.patch_log.clear();
                    self.log_base = self.emit_gen + 1;
                }
            }
            Ok(Some(Frame::new(self.width, self.height, emit)))
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

    /// Replays the logged dirty-region pixels newer than `buf`'s generation
    /// so a recycled emit buffer matches the current accumulated image.
    fn replay_patches(&self, buf: &mut [u8]) {
        // `buf` was emitted gap generations ago; every logged patch applies.
        let w = self.width as usize;
        for (_, patches) in &self.patch_log {
            for (rect, bytes) in patches {
                let mut offset = 0usize;
                let left = rect.left.max(0) as usize;
                let top = rect.top.max(0) as usize;
                let cols = (rect.right - rect.left).max(0) as usize;
                let rows = (rect.bottom - rect.top).max(0) as usize;
                for y in top..top + rows {
                    let start = (y * w + left) * 4;
                    let len = cols * 4;
                    if start + len <= buf.len() && offset + len <= bytes.len() {
                        buf[start..start + len].copy_from_slice(&bytes[offset..offset + len]);
                    }
                    offset += len;
                }
            }
        }
    }

    /// Copies each dirty rect GPU→staging (sub-resource copies, so only dirty
    /// data crosses the bus), then copies native BGRA into `accum` and the
    /// emit buffer. When `log_patches` is set the rect bytes are
    /// retained so future recycled buffers can catch up cheaply.
    fn copy_dirty(
        &mut self,
        texture: &ID3D11Texture2D,
        rects: &[RECT],
        emit: &mut [u8],
        log_patches: bool,
    ) -> Result<Option<Vec<Patch>>> {
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
            let mut logged = log_patches.then(Vec::new);
            for rect in rects {
                let left = (rect.left.max(0) as usize).min(self.width as usize);
                let right = (rect.right as usize).min(self.width as usize);
                let top = (rect.top.max(0) as usize).min(self.height as usize);
                let bottom = (rect.bottom as usize).min(self.height as usize);
                for y in top..bottom {
                    let row_bytes = (right - left) * 4;
                    let src_row =
                        std::slice::from_raw_parts(src.add(y * row_pitch + left * 4), row_bytes);
                    let dst_start = (y * self.width as usize + left) * 4;
                    let dst_end = dst_start + row_bytes;
                    self.accum[dst_start..dst_end].copy_from_slice(src_row);
                    emit[dst_start..dst_end].copy_from_slice(src_row);
                }
                if let Some(log) = &mut logged {
                    let cols = (right - left) * 4;
                    let mut bytes = Vec::with_capacity(cols * (bottom - top));
                    for y in top..bottom {
                        let start = (y * self.width as usize + left) * 4;
                        bytes.extend_from_slice(&self.accum[start..start + cols]);
                    }
                    log.push((*rect, bytes));
                }
            }
            self.context.Unmap(&self.staging, 0);
            Ok(logged)
        }
    }
}

/// Captures one full desktop frame through DXGI. This exists for startup
/// warmup paths that must not instantiate xcap's Windows Graphics Capture:
/// dropping a one-shot WGC session can let Windows unload GraphicsCapture.dll
/// while its async teardown is still active, intermittently crashing the whole
/// agent with 0xc0000005.
pub(super) fn capture_once(monitor: &Monitor) -> Result<Frame> {
    let h_monitor = HMONITOR(monitor.id()? as usize as *mut _);
    let mut dup = Duplication::try_new(h_monitor)?;
    let latest = Arc::new(LatestFrame::default());
    for _ in 0..8 {
        if let Some(frame) = dup.acquire(125, &latest)? {
            return Ok(frame);
        }
    }
    bail!("DXGI duplication produced no initial frame")
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

const RESUME_GAP: Duration = Duration::from_secs(2);

fn capture_loop_resumed(previous_loop: Instant, loop_started: Instant) -> bool {
    loop_started.duration_since(previous_loop) >= RESUME_GAP
}

fn recreate(h_monitor: HMONITOR, dup: &mut Option<Duplication>) -> bool {
    // Windows allows only one duplication of an output. Release the stale COM
    // objects before opening their replacement; creating the replacement first
    // makes every recovery fail with DuplicateOutput/access-denied.
    drop(dup.take());
    match Duplication::new(h_monitor) {
        Ok(next) => {
            *dup = Some(next);
            true
        }
        Err(recreate_error) => {
            tracing::warn!("DXGI duplication recreate failed: {recreate_error:#}");
            std::thread::sleep(Duration::from_secs(1));
            false
        }
    }
}

fn run(h_monitor_raw: usize, dup: Duplication, slot: Weak<LatestFrame>) {
    #[cfg(windows)]
    let _mmcss = crate::media::register_mmcss();
    let h_monitor = HMONITOR(h_monitor_raw as *mut _);
    let mut dup = Some(dup);
    // A process/system suspend can invalidate the desktop-duplication session
    // without returning ACCESS_LOST: after resume AcquireNextFrame only times
    // out. Do not infer staleness from a static desktop — timeouts are normal
    // there. Infer suspension from the capture loop itself missing its normal
    // ~100ms cadence by a wide margin instead.
    let mut previous_loop = Instant::now();
    let mut recreate_failures = 0_u8;
    let mut recreate_bounded = |dup: &mut Option<Duplication>| {
        if recreate(h_monitor, dup) {
            recreate_failures = 0;
            true
        } else {
            recreate_failures += 1;
            // A permanently unavailable duplication session must not leave the
            // stream on indefinite keepalives. Exit so the caller can rebuild
            // the pipeline and fall back to the continuous capture backend.
            recreate_failures < 5
        }
    };
    loop {
        let Some(latest) = slot.upgrade() else {
            return;
        };
        if latest.stopped.load(Ordering::Acquire) {
            return;
        }
        let loop_started = Instant::now();
        let resumed = capture_loop_resumed(previous_loop, loop_started);
        previous_loop = loop_started;
        if dup.is_none() && !recreate_bounded(&mut dup) {
            tracing::warn!("DXGI duplication recovery failed repeatedly; capture thread exiting");
            return;
        }
        if dup.is_none() {
            continue;
        }
        let acquire_started = Instant::now();
        let (acquired, emit_gen) = {
            let active = dup.as_mut().expect("duplication checked above");
            let acquired = active.acquire(100, &latest);
            (acquired, active.emit_gen)
        };
        let resumed = resumed || capture_loop_resumed(acquire_started, Instant::now());
        match acquired {
            Ok(Some(frame)) => {
                // Backend-verified change: producer skips the memcmp.
                if !latest.replace_hint(frame, super::PixelFormat::Bgra, Some(true), emit_gen) {
                    return;
                }
            }
            Ok(None) => {
                // Give the existing session one acquire after a resume: if
                // the compositor can still deliver accumulated changes, no
                // rebuild is needed. The stale post-suspend session observed
                // in cycle 48 only timed out, so a timeout here is the safe
                // signal to replace it.
                if resumed && !recreate_bounded(&mut dup) {
                    tracing::warn!(
                        "DXGI duplication recovery failed repeatedly; capture thread exiting"
                    );
                    return;
                }
            }
            Err(error) => {
                let is_access_lost = error
                    .downcast_ref::<windows::core::Error>()
                    .is_some_and(|e| e.code() == DXGI_ERROR_ACCESS_LOST);
                if !is_access_lost {
                    tracing::warn!(%error, "DXGI duplication capture ended");
                    return;
                }
                // Desktop switch / mode change: rebuild the duplication.
                if !recreate_bounded(&mut dup) {
                    tracing::warn!(
                        "DXGI duplication recovery failed repeatedly; capture thread exiting"
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Gdi::HMONITOR;

    static NEXT_TEST_OUTPUT: AtomicUsize = AtomicUsize::new(0x7000_0000);

    fn test_monitor() -> HMONITOR {
        HMONITOR(NEXT_TEST_OUTPUT.fetch_add(0x10, Ordering::Relaxed) as *mut _)
    }

    #[test]
    fn output_permit_rejects_overlapping_try_acquire() {
        let monitor = test_monitor();
        let first = OutputPermit::try_acquire(monitor).expect("first permit");
        assert!(OutputPermit::try_acquire(monitor).is_err());
        drop(first);
        assert!(OutputPermit::try_acquire(monitor).is_ok());
    }

    #[test]
    fn output_permit_waits_for_release() {
        let monitor = test_monitor();
        let first = OutputPermit::try_acquire(monitor).expect("first permit");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(first);
        });
        let started = Instant::now();
        let second = OutputPermit::acquire(monitor).expect("second permit");
        assert!(started.elapsed() >= Duration::from_millis(40));
        drop(second);
    }

    #[test]
    fn resume_detection_ignores_normal_static_capture_cadence() {
        let start = Instant::now();
        assert!(!capture_loop_resumed(start, start));
        assert!(!capture_loop_resumed(
            start,
            start + Duration::from_millis(100)
        ));
        assert!(!capture_loop_resumed(
            start,
            start + RESUME_GAP - Duration::from_millis(1)
        ));
    }

    #[test]
    fn resume_detection_catches_process_or_system_suspend() {
        let start = Instant::now();
        assert!(capture_loop_resumed(start, start + RESUME_GAP));
        assert!(capture_loop_resumed(start, start + Duration::from_secs(7)));
    }
}
