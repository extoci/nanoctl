use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;
use std::slice;

use anyhow::{Result, anyhow, ensure};

use crate::config::LatencyMode;
use crate::media::{EncodedFrame, FrameEncoder};

unsafe extern "C" {
    fn nanoctl_nvenc_open(
        width: u32,
        height: u32,
        bitrate: u32,
        fps: u32,
        quality: u32,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn nanoctl_nvenc_close(handle: *mut c_void);
    fn nanoctl_nvenc_set_bitrate(
        handle: *mut c_void,
        bitrate: u32,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
    fn nanoctl_nvenc_encode(
        handle: *mut c_void,
        rgba: *const u8,
        timestamp: u64,
        keyframe: bool,
        callback: unsafe extern "C" fn(*mut c_void, *const u8, usize),
        destination: *mut c_void,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
}

type ErrorBuffer = [c_char; 256];

fn native_error(error: &ErrorBuffer) -> anyhow::Error {
    // SAFETY: the zeroed buffer is only written through bounded snprintf calls.
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
    anyhow!("{message}")
}

struct Session(NonNull<c_void>);

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: self owns the live driver handle and is confined to its creation thread.
        unsafe { nanoctl_nvenc_close(self.0.as_ptr()) };
    }
}

unsafe extern "C" fn copy_bitstream(destination: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: encode passes an exclusive Vec pointer; the shim calls back synchronously while
    // the driver owns and locks a non-null bitstream buffer of at least len bytes.
    let output = unsafe { &mut *destination.cast::<Vec<u8>>() };
    output.extend_from_slice(unsafe { slice::from_raw_parts(data, len) });
}

pub struct NvencEncoder {
    session: Option<Session>,
    dimensions: (u32, u32),
    bitrate_kbps: u32,
    max_fps: u16,
    latency_mode: LatencyMode,
    timestamp: u64,
    force_keyframe: bool,
}

impl NvencEncoder {
    pub fn new(
        bitrate_kbps: u32,
        max_fps: u16,
        latency_mode: LatencyMode,
        dimensions: (u32, u32),
    ) -> Result<Self> {
        let mut encoder = Self {
            session: None,
            dimensions,
            bitrate_kbps,
            max_fps,
            latency_mode,
            timestamp: 0,
            force_keyframe: true,
        };
        encoder.reopen(dimensions)?;
        Ok(encoder)
    }

    fn reopen(&mut self, (width, height): (u32, u32)) -> Result<()> {
        ensure!(
            width > 0 && height > 0 && width.is_multiple_of(2) && height.is_multiple_of(2),
            "NVENC dimensions must be nonzero and even"
        );
        ensure!(self.max_fps > 0, "NVENC requires a positive frame rate");
        self.session.take();
        let quality = match self.latency_mode {
            LatencyMode::Responsiveness => 0,
            LatencyMode::Balanced => 1,
            LatencyMode::Quality => 2,
        };
        let mut error: ErrorBuffer = [0; 256];
        // SAFETY: the shim borrows the writable error buffer for the duration of the call.
        let handle = unsafe {
            nanoctl_nvenc_open(
                width,
                height,
                self.bitrate_kbps.saturating_mul(1_000),
                u32::from(self.max_fps),
                quality,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        self.session = Some(Session(
            NonNull::new(handle).ok_or_else(|| native_error(&error))?,
        ));
        self.dimensions = (width, height);
        self.force_keyframe = true;
        Ok(())
    }
}

impl FrameEncoder for NvencEncoder {
    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn set_bitrate(&mut self, target_kbps: u32) -> Result<()> {
        self.bitrate_kbps = target_kbps;
        let Some(session) = &self.session else {
            return self.reopen(self.dimensions);
        };
        let mut error: ErrorBuffer = [0; 256];
        // SAFETY: the session and error buffer remain valid for the duration of the call.
        let status = unsafe {
            nanoctl_nvenc_set_bitrate(
                session.0.as_ptr(),
                target_kbps.saturating_mul(1_000),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status != 0 {
            tracing::debug!(error = %native_error(&error), "recreating NVENC after an unsupported bitrate update");
            self.reopen(self.dimensions)?;
        }
        Ok(())
    }

    fn encode(&mut self, image: &crate::media::RgbaFrame<'_>) -> Result<EncodedFrame> {
        let (width, height) = image.dimensions();
        if (width, height) != self.dimensions || self.session.is_none() {
            self.reopen((width, height))?;
        }
        let session = self
            .session
            .as_ref()
            .expect("NVENC session was initialized");
        let mut error: ErrorBuffer = [0; 256];
        let mut bytes = Vec::<u8>::new();
        // SAFETY: the image has exactly the configured packed RGBA dimensions. The shim
        // borrows all buffers for this call only and copies the output through the callback.
        let status = unsafe {
            nanoctl_nvenc_encode(
                session.0.as_ptr(),
                image.as_raw().as_ptr(),
                self.timestamp,
                self.force_keyframe,
                copy_bitstream,
                std::ptr::from_mut(&mut bytes).cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status != 0 {
            return Err(native_error(&error));
        }
        self.force_keyframe = false;
        self.timestamp = self.timestamp.saturating_add(1);
        Ok(EncodedFrame {
            bytes,
            width,
            height,
        })
    }
}
