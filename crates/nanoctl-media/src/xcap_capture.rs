use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use async_trait::async_trait;
use xcap::Monitor;

use crate::types::{MediaError, PixelFormat, RawFrame, VideoSource};

#[derive(Debug, Clone, Copy)]
enum DisplaySelection {
    Primary,
    Id(u32),
}

struct CapturedFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Initial cross-platform capture backend used to bring up the clean pipeline on
/// Windows. xcap monitor handles are not thread-safe to move between arbitrary async
/// tasks, so the monitor is created and owned by one dedicated capture thread. The
/// async source only exchanges bounded request/response messages with that owner.
pub struct XcapCaptureSource {
    request_tx: Option<mpsc::Sender<()>>,
    frame_rx: tokio::sync::mpsc::Receiver<Result<CapturedFrame, String>>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
}

impl XcapCaptureSource {
    pub fn primary() -> Result<Self, MediaError> {
        Self::start(DisplaySelection::Primary)
    }

    pub fn display(display_id: u32) -> Result<Self, MediaError> {
        Self::start(DisplaySelection::Id(display_id))
    }

    fn start(selection: DisplaySelection) -> Result<Self, MediaError> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Option<u32>, String>>();
        let (request_tx, request_rx) = mpsc::channel::<()>();
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(1);
        let thread = std::thread::spawn(move || {
            capture_thread(selection, ready_tx, request_rx, frame_tx);
        });
        let display_id = ready_rx
            .recv()
            .map_err(|_| MediaError::Capture("capture worker exited before startup".to_string()))?
            .map_err(MediaError::Capture)?;
        let _ = display_id;
        Ok(Self {
            request_tx: Some(request_tx),
            frame_rx,
            thread: Some(thread),
            started: Instant::now(),
        })
    }
}

impl Drop for XcapCaptureSource {
    fn drop(&mut self) {
        self.request_tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[async_trait]
impl VideoSource for XcapCaptureSource {
    async fn next_frame(&mut self) -> Result<Option<RawFrame>, MediaError> {
        let Some(request_tx) = &self.request_tx else {
            return Ok(None);
        };
        if request_tx.send(()).is_err() {
            return Ok(None);
        }
        let Some(frame) = self.frame_rx.recv().await else {
            return Ok(None);
        };
        let frame = frame.map_err(MediaError::Capture)?;
        Ok(Some(RawFrame::new(
            self.started.elapsed().as_micros() as u64,
            frame.width,
            frame.height,
            PixelFormat::Rgba32,
            frame.pixels,
        )))
    }
}

fn capture_thread(
    selection: DisplaySelection,
    ready_tx: mpsc::Sender<Result<Option<u32>, String>>,
    request_rx: mpsc::Receiver<()>,
    frame_tx: tokio::sync::mpsc::Sender<Result<CapturedFrame, String>>,
) {
    let monitor = match select_monitor(selection) {
        Ok(monitor) => monitor,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    if ready_tx.send(Ok(monitor.id().ok())).is_err() {
        return;
    }
    while request_rx.recv().is_ok() {
        let frame = monitor
            .capture_image()
            .map(|image| CapturedFrame {
                width: image.width(),
                height: image.height(),
                pixels: image.into_raw(),
            })
            .map_err(|error| error.to_string());
        if frame_tx.blocking_send(frame).is_err() {
            return;
        }
    }
}

fn select_monitor(selection: DisplaySelection) -> Result<Monitor, String> {
    let monitors = Monitor::all().map_err(|error| error.to_string())?;
    match selection {
        DisplaySelection::Primary => monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .cloned()
            .ok_or_else(|| "no display is available".to_string()),
        DisplaySelection::Id(display_id) => monitors
            .into_iter()
            .find(|monitor| monitor.id().is_ok_and(|id| id == display_id))
            .ok_or_else(|| "selected display is unavailable".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::XcapCaptureSource;
    use crate::types::VideoSource;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[ignore = "requires a real interactive display"]
    async fn captures_primary_display_shape() {
        let mut source = XcapCaptureSource::primary().unwrap();
        let frame = source.next_frame().await.unwrap().unwrap();
        assert!(frame.width > 0);
        assert!(frame.height > 0);
        assert_eq!(
            frame.pixels.len(),
            frame.width as usize * frame.height as usize * 4
        );
    }
}
