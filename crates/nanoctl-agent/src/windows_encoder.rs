// Windows hardware H.264 encoding through an asynchronous Media Foundation Transform.
//
// Only transforms returned by `MFT_ENUM_FLAG_HARDWARE` are accepted. The inbox software MFT is
// deliberately not used here: `quality.encoder = "hardware"` must never be a mislabeled software
// path, while `auto` can still fall back to OpenH264 in `media.rs`.

use std::mem::ManuallyDrop;
use std::ptr;
use std::slice;
use std::time::{Duration, Instant};

use crate::config::LatencyMode;
use crate::media::{EncodedFrame, bitrate_target};
use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::Interface;

struct MediaFoundationRuntime;

impl MediaFoundationRuntime {
    fn start() -> Result<Self> {
        // SAFETY: this worker owns its COM apartment and balances both successful initializations
        // in Drop. `spawn_video` constructs and uses the encoder on the same blocking thread.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("cannot initialize the Media Foundation COM apartment")?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error).context("cannot start Media Foundation");
            }
        }
        Ok(Self)
    }
}

impl Drop for MediaFoundationRuntime {
    fn drop(&mut self) {
        // SAFETY: paired with successful calls in `start` and dropped on the owning thread.
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

struct ActiveEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    width: u32,
    height: u32,
    output_buffer_size: u32,
    provides_output_samples: bool,
    needs_input: bool,
}

pub struct WindowsEncoder {
    _runtime: MediaFoundationRuntime,
    active: Option<ActiveEncoder>,
    bitrate_kbps: u32,
    max_fps: u16,
    rebuild: bool,
    timestamp: i64,
    nv12: Vec<u8>,
}

impl WindowsEncoder {
    pub fn new(bitrate_kbps: u32, max_fps: u16, _latency_mode: LatencyMode) -> Result<Self> {
        let runtime = MediaFoundationRuntime::start()?;
        let mut encoder = Self {
            _runtime: runtime,
            active: None,
            bitrate_kbps,
            max_fps,
            rebuild: false,
            timestamp: 0,
            nv12: Vec::new(),
        };
        // `doctor` calls this constructor. Configure a real encoder rather than treating an
        // enumeration result as proof that its media types and buffers work.
        encoder.active = Some(encoder.create_active(64, 64)?);
        Ok(encoder)
    }

    pub fn force_keyframe(&mut self) {
        // Rebuilding a low-latency transform starts a new GOP and avoids relying on optional
        // vendor-specific ICodecAPI force-keyframe support.
        self.rebuild = true;
    }

    pub fn apply_bitrate_estimate(
        &mut self,
        estimate_kbps: u32,
        ceiling_kbps: u32,
    ) -> Result<bool> {
        let floor_kbps = 250.min(ceiling_kbps);
        let Some(target_kbps) =
            bitrate_target(self.bitrate_kbps, estimate_kbps, floor_kbps, ceiling_kbps)
        else {
            return Ok(false);
        };
        self.bitrate_kbps = target_kbps;
        self.rebuild = true;
        Ok(true)
    }

    pub fn encode(&mut self, image: &xcap::image::RgbaImage) -> Result<EncodedFrame> {
        let width = image.width();
        let height = image.height();
        let dimensions_changed = self
            .active
            .as_ref()
            .is_none_or(|active| active.width != width || active.height != height);
        if self.rebuild || dimensions_changed {
            self.active = Some(self.create_active(width, height)?);
            self.rebuild = false;
        }
        rgba_to_nv12(image.as_raw(), width, height, &mut self.nv12)?;
        let duration = 10_000_000_i64 / i64::from(self.max_fps);
        let sample = create_input_sample(&self.nv12, self.timestamp, duration)?;
        self.timestamp = self.timestamp.saturating_add(duration);
        let active = self
            .active
            .as_mut()
            .context("Media Foundation encoder was not initialized")?;
        wait_until_input_needed(active)?;

        // SAFETY: stream zero and the sample are configured for this transform; all pointer
        // ownership is held by projected COM wrappers.
        unsafe {
            active
                .transform
                .ProcessInput(0, &sample, 0)
                .context("Media Foundation rejected an NV12 frame")?;
        }
        active.needs_input = false;
        let bytes = wait_for_output(active)?;
        validate_annex_b(&bytes)?;
        Ok(EncodedFrame {
            bytes,
            width,
            height,
        })
    }

    fn create_active(&self, width: u32, height: u32) -> Result<ActiveEncoder> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            bail!("Media Foundation H.264 dimensions must be non-zero and even");
        }
        let transform = enumerate_hardware_encoder()?;
        let attributes = unsafe {
            transform
                .GetAttributes()
                .context("hardware MFT has no attribute store")?
        };
        unsafe {
            attributes
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .context("cannot unlock asynchronous hardware MFT")?;
            attributes
                .SetUINT32(&MF_LOW_LATENCY, 1)
                .context("hardware MFT does not support required low-latency mode")?;
        }
        let events: IMFMediaEventGenerator = transform
            .cast()
            .context("hardware MFT has no asynchronous event interface")?;
        let output = create_video_type(
            &MFVideoFormat_H264,
            width,
            height,
            self.max_fps,
            Some(self.bitrate_kbps.saturating_mul(1_000)),
        )?;
        // SAFETY: output-before-input is required by the Microsoft H.264 encoder contract.
        unsafe {
            output
                .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)
                .context("cannot request H.264 baseline profile")?;
            transform
                .SetOutputType(0, &output, 0)
                .context("hardware MFT rejected H.264 output settings")?;
        }
        let input = create_video_type(&MFVideoFormat_NV12, width, height, self.max_fps, None)?;
        // SAFETY: types are complete and stream IDs are zero for an encoder MFT.
        unsafe {
            transform
                .SetInputType(0, &input, 0)
                .context("hardware MFT rejected NV12 input settings")?;
            transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .context("cannot flush hardware MFT")?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .context("cannot begin hardware MFT streaming")?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .context("cannot start hardware MFT stream")?;
        }
        let output_info = unsafe {
            transform
                .GetOutputStreamInfo(0)
                .context("cannot query hardware MFT output buffer")?
        };
        let pixel_bound = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(2))
            .context("Media Foundation frame dimensions overflow")?;
        let output_buffer_size = output_info.cbSize.max(pixel_bound).max(64 * 1024);
        let provides_output_samples =
            output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        let mut active = ActiveEncoder {
            transform,
            events,
            width,
            height,
            output_buffer_size,
            provides_output_samples,
            needs_input: false,
        };
        wait_until_input_needed(&mut active)?;
        Ok(active)
    }
}

fn enumerate_hardware_encoder() -> Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let mut activations: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0_u32;
    // SAFETY: Media Foundation allocates an array of `count` initialized interface pointers.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            &mut activations,
            &mut count,
        )
        .context("cannot enumerate Media Foundation hardware encoders")?;
    }
    if activations.is_null() {
        bail!("no NV12-to-H.264 hardware MFT is installed");
    }
    if count == 0 {
        unsafe {
            CoTaskMemFree(Some(activations.cast()));
        }
        bail!("no NV12-to-H.264 hardware MFT is installed");
    }
    // SAFETY: the successful call above returned exactly `count` slots. Taking each Option moves
    // ownership of its COM reference before the backing pointer array is freed.
    let available = unsafe { slice::from_raw_parts_mut(activations, count as usize) };
    let owned: Vec<IMFActivate> = available.iter_mut().filter_map(Option::take).collect();
    unsafe {
        CoTaskMemFree(Some(activations.cast()));
    }
    let mut last_error = None;
    for activation in owned {
        let activated = unsafe { activation.ActivateObject::<IMFTransform>() };
        match activated {
            Ok(transform) => return Ok(transform),
            Err(error) => last_error = Some(error),
        }
    }
    Err(anyhow!(
        "Media Foundation hardware encoders could not be activated: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no activation object".to_owned())
    ))
}

fn create_video_type(
    subtype: &windows::core::GUID,
    width: u32,
    height: u32,
    fps: u16,
    bitrate: Option<u32>,
) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType().context("cannot allocate a video media type")? };
    let packed_size = (u64::from(width) << 32) | u64::from(height);
    let packed_rate = (u64::from(fps) << 32) | 1;
    // SAFETY: all attributes use the types required by the Media Foundation H.264 contract.
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, packed_size)?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, packed_rate)?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1)?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if let Some(bitrate) = bitrate {
            media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate.max(1))?;
        }
    }
    Ok(media_type)
}

fn create_input_sample(bytes: &[u8], timestamp: i64, duration: i64) -> Result<IMFSample> {
    let length = u32::try_from(bytes.len()).context("NV12 frame is too large")?;
    // SAFETY: the locked buffer is valid for `length` bytes and is always unlocked before return.
    unsafe {
        let buffer =
            MFCreateMemoryBuffer(length).context("cannot allocate an NV12 media buffer")?;
        let mut destination = ptr::null_mut();
        buffer
            .Lock(&mut destination, None, None)
            .context("cannot lock an NV12 media buffer")?;
        ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
        if let Err(error) = buffer.Unlock() {
            return Err(error).context("cannot unlock an NV12 media buffer");
        }
        buffer
            .SetCurrentLength(length)
            .context("cannot set NV12 media buffer length")?;
        let sample = MFCreateSample().context("cannot allocate an input media sample")?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(timestamp)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

fn wait_until_input_needed(active: &mut ActiveEncoder) -> Result<()> {
    if active.needs_input {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match next_transform_event(active)? {
            Some(event_type) if event_type == METransformNeedInput.0 as u32 => {
                active.needs_input = true;
                return Ok(());
            }
            Some(event_type) if event_type == METransformHaveOutput.0 as u32 => {
                let _ = pull_output(active)?;
            }
            Some(_) | None => {}
        }
    }
    bail!("hardware MFT did not request input before the watchdog deadline")
}

fn wait_for_output(active: &mut ActiveEncoder) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match next_transform_event(active)? {
            Some(event_type) if event_type == METransformHaveOutput.0 as u32 => {
                return pull_output(active);
            }
            Some(event_type) if event_type == METransformNeedInput.0 as u32 => {
                active.needs_input = true;
            }
            Some(_) | None => {}
        }
    }
    bail!("hardware MFT produced no output before the watchdog deadline")
}

fn next_transform_event(active: &ActiveEncoder) -> Result<Option<u32>> {
    let event = unsafe { active.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
    match event {
        Ok(event) => {
            let status = unsafe { event.GetStatus()? };
            status
                .ok()
                .context("hardware MFT reported a failed media event")?;
            Ok(Some(unsafe { event.GetType()? }))
        }
        Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => {
            std::thread::sleep(Duration::from_millis(1));
            Ok(None)
        }
        Err(error) => Err(error).context("cannot read a hardware MFT event"),
    }
}

fn pull_output(active: &mut ActiveEncoder) -> Result<Vec<u8>> {
    let supplied_sample = if active.provides_output_samples {
        None
    } else {
        // A conservative buffer bound prevents a driver-reported zero size from creating an empty
        // output buffer.
        let sample = unsafe { MFCreateSample().context("cannot allocate an output media sample")? };
        let buffer = unsafe {
            MFCreateMemoryBuffer(active.output_buffer_size)
                .context("cannot allocate an H.264 output buffer")?
        };
        unsafe {
            sample.AddBuffer(&buffer)?;
        }
        Some(sample)
    };
    let mut output = [MFT_OUTPUT_DATA_BUFFER {
        dwStreamID: 0,
        pSample: ManuallyDrop::new(supplied_sample),
        dwStatus: 0,
        pEvents: ManuallyDrop::new(None),
    }];
    let mut status = 0_u32;
    let result = unsafe { active.transform.ProcessOutput(0, &mut output, &mut status) };
    if let Err(error) = result {
        release_output_slots(&mut output);
        if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
            bail!("low-latency hardware MFT buffered a frame instead of producing output");
        }
        return Err(error).context("Media Foundation hardware encode failed");
    }
    // SAFETY: the slot was initialized above and remains owned by this function.
    let sample = unsafe { ManuallyDrop::take(&mut output[0].pSample) }
        .context("hardware MFT returned no H.264 sample")?;
    // SAFETY: discard any events returned by the transform while retaining correct COM release.
    let _events = unsafe { ManuallyDrop::take(&mut output[0].pEvents) };
    let contiguous = unsafe {
        sample
            .ConvertToContiguousBuffer()
            .context("cannot flatten H.264 output buffers")?
    };
    let length = unsafe { contiguous.GetCurrentLength()? } as usize;
    if length == 0 || length > active.output_buffer_size as usize {
        bail!("hardware MFT returned an invalid H.264 output length");
    }
    let mut source = ptr::null_mut();
    unsafe {
        contiguous.Lock(&mut source, None, None)?;
    }
    let bytes = unsafe { slice::from_raw_parts(source, length) }.to_vec();
    unsafe {
        contiguous.Unlock()?;
    }
    Ok(bytes)
}

fn release_output_slots(output: &mut [MFT_OUTPUT_DATA_BUFFER]) {
    for slot in output {
        // SAFETY: each ManuallyDrop field is initialized exactly once by this module.
        unsafe {
            let _ = ManuallyDrop::take(&mut slot.pSample);
            let _ = ManuallyDrop::take(&mut slot.pEvents);
        }
    }
}

fn validate_annex_b(bytes: &[u8]) -> Result<()> {
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        Ok(())
    } else {
        bail!("hardware MFT produced H.264 without Annex-B start codes")
    }
}

fn rgba_to_nv12(rgba: &[u8], width: u32, height: u32, output: &mut Vec<u8>) -> Result<()> {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        bail!("NV12 dimensions must be non-zero and even");
    }
    let pixels = width
        .checked_mul(height)
        .context("frame dimensions overflow")?;
    if rgba.len() != pixels.checked_mul(4).context("RGBA frame size overflow")? {
        bail!("RGBA frame length does not match its dimensions");
    }
    output.clear();
    output.resize(pixels + pixels / 2, 0);
    for row in 0..height {
        for column in 0..width {
            let offset = (row * width + column) * 4;
            let r = i32::from(rgba[offset]);
            let g = i32::from(rgba[offset + 1]);
            let b = i32::from(rgba[offset + 2]);
            output[row * width + column] =
                (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
        }
    }
    let uv_offset = pixels;
    for row in (0..height).step_by(2) {
        for column in (0..width).step_by(2) {
            let mut r = 0_i32;
            let mut g = 0_i32;
            let mut b = 0_i32;
            for y in 0..2 {
                for x in 0..2 {
                    let offset = ((row + y) * width + column + x) * 4;
                    r += i32::from(rgba[offset]);
                    g += i32::from(rgba[offset + 1]);
                    b += i32::from(rgba[offset + 2]);
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let chroma = uv_offset + (row / 2) * width + column;
            output[chroma] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            output[chroma + 1] =
                (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{rgba_to_nv12, validate_annex_b};

    #[test]
    fn validates_only_annex_b_access_units() {
        assert!(validate_annex_b(&[0, 0, 0, 1, 0x67]).is_ok());
        assert!(validate_annex_b(&[0, 0, 1, 0x65]).is_ok());
        assert!(validate_annex_b(&[0, 0, 0, 2, 0x65]).is_err());
    }

    #[test]
    fn converts_rgba_to_nv12() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut output = Vec::new();
        rgba_to_nv12(&rgba, 2, 2, &mut output).unwrap();
        assert_eq!(output, [16, 235, 16, 235, 128, 128]);
    }
}
