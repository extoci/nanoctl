//! ScreenCaptureKit display capture into the encoder's latest-frame slot.

use super::LatestFrame;
use anyhow::{Context, Result, ensure};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetDataSize,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};
use std::{
    ptr::NonNull,
    sync::{Arc, Weak, atomic::Ordering, mpsc},
    time::Duration,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NanoctlScreenCaptureOutput"]
    #[ivars = Weak<LatestFrame>]
    struct ScreenOutput;

    unsafe impl NSObjectProtocol for ScreenOutput {}

    unsafe impl SCStreamOutput for ScreenOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output_sample_buffer(
            &self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen
                && let Some(latest) = self.ivars().upgrade()
                && !latest.stopped.load(Ordering::Acquire)
                && let Some(frame) = copy_frame(sample)
                && !latest.stopped.load(Ordering::Acquire)
            {
                latest.replace(frame);
            }
        }
    }

    unsafe impl SCStreamDelegate for ScreenOutput {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stream_did_stop(&self, _stream: &SCStream, err: &NSError) {
            tracing::warn!(%err, "ScreenCaptureKit stream stopped");
            if let Some(latest) = self.ivars().upgrade() {
                let _slot = latest.slot.lock();
                latest.finished.store(true, Ordering::Release);
                latest.ready.notify_one();
            }
        }
    }
);

fn frame_status(sample: &CMSampleBuffer) -> Option<SCFrameStatus> {
    // ScreenCaptureKit attaches one CFDictionary to the sample. CFArray and
    // CFDictionary are toll-free bridged to NSArray and NSDictionary.
    unsafe {
        let attachments = sample.sample_attachments_array(false)?;
        let array = &*std::ptr::from_ref(&*attachments)
            .cast::<NSArray<NSDictionary<NSString, AnyObject>>>();
        if array.count() == 0 {
            return None;
        }
        let status = array
            .objectAtIndex(0)
            .objectForKey(SCStreamFrameInfoStatus)?;
        let status = status.downcast::<NSNumber>().ok()?;
        Some(SCFrameStatus(status.integerValue()))
    }
}

fn copy_frame(sample: &CMSampleBuffer) -> Option<xcap::Frame> {
    if !matches!(
        frame_status(sample)?,
        SCFrameStatus::Complete | SCFrameStatus::Started
    ) {
        return None;
    }
    // The callback owns the sample for this call. Read only while its buffer is locked.
    unsafe {
        let pixels = CMSampleBuffer::image_buffer(sample)?;
        let access = CVPixelBufferLockFlags::ReadOnly;
        if CVPixelBufferGetPixelFormatType(&pixels) != kCVPixelFormatType_32BGRA
            || CVPixelBufferLockBaseAddress(&pixels, access) != 0
        {
            return None;
        }
        let frame = (|| {
            let width = CVPixelBufferGetWidth(&pixels).try_into().ok()?;
            let height = CVPixelBufferGetHeight(&pixels).try_into().ok()?;
            let stride = CVPixelBufferGetBytesPerRow(&pixels);
            let base = NonNull::new(CVPixelBufferGetBaseAddress(&pixels))?;
            bgra_to_rgba(
                width,
                height,
                stride,
                base.as_ptr().cast::<u8>(),
                CVPixelBufferGetDataSize(&pixels),
            )
        })();
        CVPixelBufferUnlockBaseAddress(&pixels, access);
        frame
    }
}

// SAFETY: The supplied size must describe a live allocation at base, with all
// visible pixels initialized. Row padding does not need to be initialized.
unsafe fn bgra_to_rgba(
    width: u32,
    height: u32,
    stride: usize,
    base: *const u8,
    size: usize,
) -> Option<xcap::Frame> {
    let row_bytes = (width as usize).checked_mul(4)?;
    let height_usize = height as usize;
    if row_bytes == 0 || height_usize == 0 || stride < row_bytes {
        return None;
    }
    let visible_end = stride
        .checked_mul(height_usize - 1)?
        .checked_add(row_bytes)?;
    if visible_end > size || visible_end > isize::MAX as usize {
        return None;
    }
    let mut raw = Vec::with_capacity(row_bytes * height_usize);
    if row_bytes == stride {
        raw.extend_from_slice(unsafe { std::slice::from_raw_parts(base, visible_end) });
    } else {
        for row in 0..height_usize {
            raw.extend_from_slice(unsafe {
                std::slice::from_raw_parts(base.add(row * stride), row_bytes)
            });
        }
    }
    for pixel in raw.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(xcap::Frame::new(width, height, raw))
}

pub(super) struct ScreenRecorder {
    stream: Retained<SCStream>,
    _output: Retained<ScreenOutput>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl ScreenRecorder {
    pub(super) fn supported() -> bool {
        // Keep click highlights on macOS 14, where only AVCapture can draw them.
        autoreleasepool(|_| unsafe {
            SCStreamConfiguration::new().respondsToSelector(sel!(setShowMouseClicks:))
        })
    }

    pub(super) fn start(display_id: u32, latest: &Arc<LatestFrame>) -> Result<Self> {
        autoreleasepool(|_| {
            let display = find_display(display_id)?;
            let mode =
                CGDisplayCopyDisplayMode(display_id).context("display mode is unavailable")?;
            let width = CGDisplayMode::pixel_width(Some(&mode));
            let height = CGDisplayMode::pixel_height(Some(&mode));
            ensure!(width > 0 && height > 0, "invalid display dimensions");

            // All ObjC objects stay alive through these calls; the callback only
            // shares the thread-safe latest-frame slot with the capture worker.
            let recorder = unsafe {
                let filter = SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display.0,
                    &NSArray::new(),
                );
                let config = SCStreamConfiguration::new();
                config.setWidth(width);
                config.setHeight(height);
                config.setPixelFormat(kCVPixelFormatType_32BGRA);
                config.setMinimumFrameInterval(CMTime::new(1, 60));
                config.setQueueDepth(3);
                config.setShowsCursor(true);
                config.setShowMouseClicks(true);
                let this = ScreenOutput::alloc().set_ivars(Arc::downgrade(latest));
                let output: Retained<ScreenOutput> = msg_send![super(this), init];
                let queue = DispatchQueue::new("nanoctl.capture.screen", DispatchQueueAttr::SERIAL);
                let stream = SCStream::initWithFilter_configuration_delegate(
                    SCStream::alloc(),
                    &filter,
                    &config,
                    Some(ProtocolObject::from_ref(&*output)),
                );
                stream
                    .addStreamOutput_type_sampleHandlerQueue_error(
                        ProtocolObject::from_ref(&*output),
                        SCStreamOutputType::Screen,
                        Some(&queue),
                    )
                    .map_err(|err| anyhow::anyhow!("{err}"))?;
                Self {
                    stream,
                    _output: output,
                    _queue: queue,
                }
            };
            completion(|done| unsafe {
                recorder
                    .stream
                    .startCaptureWithCompletionHandler(Some(done))
            })
            .context("starting ScreenCaptureKit")?;
            let slot = latest
                .slot
                .lock()
                .map_err(|_| anyhow::anyhow!("display capture queue was poisoned"))?;
            let (slot, _) = latest
                .ready
                .wait_timeout_while(slot, CAPTURE_TIMEOUT, |frame| {
                    frame.is_none() && !latest.finished.load(Ordering::Acquire)
                })
                .map_err(|_| anyhow::anyhow!("display capture queue was poisoned"))?;
            ensure!(
                slot.is_some(),
                "ScreenCaptureKit did not deliver an initial frame"
            );
            drop(slot);
            Ok(recorder)
        })
    }
}

impl Drop for ScreenRecorder {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            // Keep the stream, output and queue alive until stop completes.
            if let Err(error) = completion(|done| unsafe {
                self.stream.stopCaptureWithCompletionHandler(Some(done));
            }) {
                tracing::debug!(%error, "stopping ScreenCaptureKit");
            }
        });
    }
}

struct ShareableDisplay(Retained<SCDisplay>);

// SAFETY: SCDisplay is an immutable display descriptor. Only the retained
// descriptor is sent; the shareable-content arrays stay on Apple's callback.
unsafe impl Send for ShareableDisplay {}

fn find_display(display_id: u32) -> Result<ShareableDisplay> {
    let (ready, result) = mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // Both parameters are valid for the lifetime of this callback.
            let display = unsafe { content.as_ref() }
                .ok_or_else(|| error_message(error))
                .and_then(|content| {
                    // Read the immutable shareable-content snapshot to locate this display.
                    unsafe {
                        let displays = content.displays();
                        (0..displays.count())
                            .map(|i| displays.objectAtIndex(i))
                            .find(|display| display.displayID() == display_id)
                            .map(ShareableDisplay)
                            .ok_or_else(|| "ScreenCaptureKit display is unavailable".to_owned())
                    }
                });
            let _ = ready.send(display);
        },
    );
    // ScreenCaptureKit copies the completion block and owns both callback arguments.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, true, &handler);
    }
    result
        .recv_timeout(CAPTURE_TIMEOUT)
        .context("ScreenCaptureKit display lookup timed out")?
        .map_err(anyhow::Error::msg)
}

fn error_message(error: *mut NSError) -> String {
    // The completion handler owns this error until it returns.
    unsafe { error.as_ref() }
        .map(ToString::to_string)
        .unwrap_or_else(|| "ScreenCaptureKit returned no result".to_owned())
}

fn completion(call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<()> {
    let (ready, result) = mpsc::channel();
    let handler = RcBlock::new(move |error: *mut NSError| {
        let _ = ready.send(if error.is_null() {
            Ok(())
        } else {
            Err(error_message(error))
        });
    });
    call(&handler);
    result
        .recv_timeout(CAPTURE_TIMEOUT)
        .context("ScreenCaptureKit operation timed out")?
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::bgra_to_rgba;

    fn convert(width: u32, height: u32, stride: usize, pixels: &[u8]) -> Option<xcap::Frame> {
        unsafe { bgra_to_rgba(width, height, stride, pixels.as_ptr(), pixels.len()) }
    }

    #[test]
    fn copies_bgra_rows_without_padding() {
        let bgra = [1, 2, 3, 4, 0xaa, 5, 6, 7, 8, 0xbb];
        assert_eq!(
            convert(1, 2, 5, &bgra).unwrap().raw,
            [3, 2, 1, 4, 7, 6, 5, 8]
        );
    }

    #[test]
    fn rejects_incomplete_pixel_buffers() {
        assert!(convert(1, 2, 4, &[0; 7]).is_none());
        assert!(convert(1, 2, 3, &[0; 8]).is_none());
    }
}
