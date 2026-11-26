// Windows hardware video encoding through an asynchronous Media Foundation Transform.
//
// Only transforms returned by `MFT_ENUM_FLAG_HARDWARE` are accepted. The inbox software MFT is
// deliberately not used here: `quality.encoder = "hardware"` must never be a mislabeled software
// path. Software fallback selection belongs above this backend, not inside it.

use std::mem::ManuallyDrop;
use std::ptr;
use std::slice;
use std::time::{Duration, Instant};

use std::sync::mpsc;
use std::thread::JoinHandle;

use async_trait::async_trait;
use nanoctl_core::video::VideoCodec;

use crate::bitstream::{has_parameter_sets, is_annex_b, is_keyframe};
use crate::nv12::frame_to_nv12_slice;
use crate::types::{EncodeRequest, EncodedBitstream, MediaError, VideoEncoder};
use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4,
};
use windows::core::Interface;

const MFT_STARTUP_BUDGET: Duration = Duration::from_secs(2);
const MFT_FRAME_BUDGET: Duration = Duration::from_millis(500);

struct MediaFoundationRuntime;

impl MediaFoundationRuntime {
    fn start() -> Result<Self> {
        // SAFETY: the encoder worker owns its COM apartment and balances both successful
        // initializations in Drop on that same thread.
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
    activation: IMFActivate,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    width: u32,
    height: u32,
    output_buffer_size: u32,
    provides_output_samples: bool,
    needs_input: bool,
    first_access_unit: bool,
    shut_down: bool,
}

impl ActiveEncoder {
    fn request_keyframe(&self) -> Result<()> {
        let codec: ICodecAPI = self
            .transform
            .cast()
            .context("hardware MFT does not expose ICodecAPI")?;
        let value = ui4_variant(1);
        unsafe {
            codec
                .IsSupported(&CODECAPI_AVEncVideoForceKeyFrame)
                .context("hardware MFT does not support forced keyframes")?;
            codec
                .SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value)
                .context("hardware MFT rejected a forced keyframe")?;
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        if self.shut_down {
            return Ok(());
        }
        // Buffered output is intentionally discarded when a desktop session ends or the encoder
        // is reconfigured. Notify and flush before deactivating the object so vendor MFTs release
        // their hardware session synchronously.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            self.activation
                .ShutdownObject()
                .context("cannot shut down hardware MFT activation")?;
        }
        self.shut_down = true;
        Ok(())
    }
}

impl Drop for ActiveEncoder {
    fn drop(&mut self) {
        // Best-effort fallback for early returns. Normal replacement calls `shutdown` explicitly
        // and refuses to activate a second hardware session if the old one cannot be released.
        let _ = self.shutdown();
    }
}

struct ActivatedTransformGuard {
    activation: Option<IMFActivate>,
    transform: Option<IMFTransform>,
}

impl ActivatedTransformGuard {
    fn new(activation: IMFActivate, transform: IMFTransform) -> Self {
        Self {
            activation: Some(activation),
            transform: Some(transform),
        }
    }

    fn transform(&self) -> &IMFTransform {
        self.transform
            .as_ref()
            .expect("activated transform guard always owns a transform")
    }

    fn into_parts(mut self) -> (IMFActivate, IMFTransform) {
        let activation = self
            .activation
            .take()
            .expect("activated transform guard always owns an activation");
        let transform = self
            .transform
            .take()
            .expect("activated transform guard always owns a transform");
        (activation, transform)
    }
}

impl Drop for ActivatedTransformGuard {
    fn drop(&mut self) {
        // Armed immediately after ActivateObject: every later setup failure deactivates the MFT.
        if let Some(transform) = &self.transform {
            unsafe {
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            }
        }
        if let Some(activation) = &self.activation {
            unsafe {
                let _ = activation.ShutdownObject();
            }
        }
    }
}

struct MftEncoder {
    _runtime: MediaFoundationRuntime,
    codec: VideoCodec,
    active: Option<ActiveEncoder>,
    bitrate_bps: u32,
    max_fps: u16,
    reconfigure: bool,
    timestamp: i64,
}

impl Drop for MftEncoder {
    fn drop(&mut self) {
        // Release the transform and its event generator before MediaFoundationRuntime calls
        // MFShutdown. The capture worker is reused across sessions, so relying on field-drop
        // ordering here makes a close→reopen race unnecessarily easy to trigger.
        if let Some(mut active) = self.active.take() {
            let _ = active.shutdown();
        }
    }
}

impl MftEncoder {
    fn new(
        codec: VideoCodec,
        bitrate_bps: u32,
        max_fps: u16,
        (probe_width, probe_height): (u32, u32),
    ) -> Result<Self> {
        if codec == VideoCodec::Av1 {
            bail!("AV1 Media Foundation encoding is not implemented yet");
        }
        let runtime = MediaFoundationRuntime::start()?;
        let mut encoder = Self {
            _runtime: runtime,
            codec,
            active: None,
            bitrate_bps,
            max_fps,
            reconfigure: false,
            timestamp: 0,
        };
        // Configure a real encoder rather than treating enumeration as proof that its media
        // types and buffers work.
        // A tiny synthetic probe is rejected by several otherwise usable hardware MFTs. Probe
        // with the actual (already capped) capture dimensions so the service does not silently
        // reject the backend before the first real frame.
        encoder.recreate_active(probe_width, probe_height)?;
        Ok(encoder)
    }
}

impl MftEncoder {
    fn set_bitrate(&mut self, target_bps: u32) -> Result<()> {
        self.bitrate_bps = target_bps;
        self.reconfigure = !self.active.as_ref().is_some_and(|active| {
            let Ok(codec) = active.transform.cast::<ICodecAPI>() else {
                return false;
            };
            let mean = ui4_variant(target_bps.max(1));
            let peak = ui4_variant(target_bps.saturating_mul(2));
            // SAFETY: the live encoder and scalar VARIANTs are valid for these calls.
            // MaxBitRate is optional on vendor MFTs: only MeanBitRate decides
            // whether the change applied in place.
            unsafe {
                let applied = codec
                    .SetValue(&CODECAPI_AVEncCommonMeanBitRate, &mean)
                    .is_ok();
                if applied {
                    let _ = codec.SetValue(&CODECAPI_AVEncCommonMaxBitRate, &peak);
                }
                applied
            }
        });
        Ok(())
    }

    fn encode_blocking(&mut self, request: &EncodeRequest) -> Result<EncodedBitstream> {
        if request.codec != self.codec {
            bail!("encoder codec changed without recreating the encoder");
        }
        if request.target_bitrate != self.bitrate_bps {
            self.set_bitrate(request.target_bitrate)?;
        }
        let width = request.frame.width;
        let height = request.frame.height;
        let dimensions_changed = self
            .active
            .as_ref()
            .is_none_or(|active| active.width != width || active.height != height);
        if self.reconfigure || dimensions_changed {
            self.recreate_active(width, height)?;
            self.reconfigure = false;
        } else if request.force_keyframe {
            let forced_in_place = self
                .active
                .as_ref()
                .is_some_and(|active| active.request_keyframe().is_ok());
            if !forced_in_place {
                // Hardware keyframe control is optional. Recreate only as a compatibility fallback,
                // and release the old MFT before activating its replacement.
                self.recreate_active(width, height)?;
            }
        }
        let pixels = (width as usize)
            .checked_mul(height as usize)
            .context("NV12 frame dimensions overflow")?;
        let nv12_len = pixels
            .checked_add(pixels / 2)
            .context("NV12 frame size overflows")?;
        let duration = 10_000_000_i64 / i64::from(self.max_fps.max(1));
        let sample = create_input_sample(nv12_len, self.timestamp, duration, |output| {
            frame_to_nv12_slice(&request.frame, output).map_err(|error| anyhow!(error.to_string()))
        })?;
        self.timestamp = self.timestamp.saturating_add(duration);
        let active = self
            .active
            .as_mut()
            .context("Media Foundation encoder was not initialized")?;
        let deadline = Instant::now() + MFT_FRAME_BUDGET;
        wait_until_input_needed(active, deadline)?;

        // SAFETY: stream zero and the sample are configured for this transform; all pointer
        // ownership is held by projected COM wrappers.
        unsafe {
            active
                .transform
                .ProcessInput(0, &sample, 0)
                .context("Media Foundation rejected an NV12 frame")?;
        }
        active.needs_input = false;
        let mut bytes = wait_for_output(active, deadline)?;
        validate_annex_b(&bytes)?;
        if std::mem::take(&mut active.first_access_unit)
            && !has_parameter_sets(self.codec, &bytes)
            && let Some(mut headers) = sequence_header(self.codec, &active.transform)
        {
            headers.append(&mut bytes);
            bytes = headers;
        }
        Ok(EncodedBitstream {
            keyframe: is_keyframe(self.codec, &bytes),
            payload: bytes,
        })
    }
}

impl MftEncoder {
    fn recreate_active(&mut self, width: u32, height: u32) -> Result<()> {
        // `Option::take` is deliberately a separate statement: evaluating `Some(create_active())`
        // first leaves two hardware MFT sessions alive at once and fails on single-session GPUs.
        if let Some(mut active) = self.active.take() {
            active
                .shutdown()
                .context("cannot release previous hardware MFT before replacement")?;
        }
        self.active = Some(self.create_active(width, height)?);
        Ok(())
    }

    fn create_active(&self, width: u32, height: u32) -> Result<ActiveEncoder> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            bail!("Media Foundation encoder dimensions must be non-zero and even");
        }
        let mut last_error = anyhow!(
            "no NV12-to-{} hardware MFT is installed",
            codec_label(self.codec)
        );
        for activation in enumerate_hardware_encoders(self.codec)? {
            // SAFETY: an enumerated encoder activation is valid on the current COM thread.
            let activated = unsafe { activation.ActivateObject::<IMFTransform>() };
            match activated {
                Ok(transform) => match self.configure_active(
                    ActivatedTransformGuard::new(activation, transform),
                    width,
                    height,
                ) {
                    Ok(active) => return Ok(active),
                    Err(error) => last_error = error,
                },
                Err(error) => last_error = error.into(),
            }
        }
        Err(last_error).context(format!(
            "no hardware MFT accepted the {} settings",
            codec_label(self.codec)
        ))
    }

    fn configure_active(
        &self,
        activated: ActivatedTransformGuard,
        width: u32,
        height: u32,
    ) -> Result<ActiveEncoder> {
        let transform = activated.transform();
        let attributes = unsafe {
            transform
                .GetAttributes()
                .context("hardware MFT has no attribute store")?
        };
        unsafe {
            if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                attributes
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                    .context("cannot unlock asynchronous hardware MFT")?;
            }
            // MF_LOW_LATENCY is an optional encoder capability. Requiring every vendor MFT to
            // accept the pipeline attribute made valid hardware encoders look unavailable; the
            // encoder still uses no frame reordering and a bounded output wait below.
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
        }
        if let Ok(codec) = transform.cast::<ICodecAPI>() {
            let low_latency = VARIANT {
                Anonymous: VARIANT_0 {
                    Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                        vt: VT_BOOL,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: VARIANT_0_0_0 {
                            boolVal: VARIANT_TRUE,
                        },
                    }),
                },
            };
            // SAFETY: these optional codec properties take scalar variants.
            unsafe {
                let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
                let _ = codec.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &ui4_variant(0));
                // Unrepairable IDRs already force a keyframe through the
                // RTCP loss path, so periodic IDRs are pure overhead. Vendor
                // MFTs default to ~2s; best-effort.
                let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &ui4_variant(u32::MAX));
                // Peak-constrained VBR lets easy frames undershoot the
                // target instead of padding to it; MeanBitRate stays the
                // average cap. Vendor MFTs without the property keep CBR.
                let _ = codec.SetValue(
                    &CODECAPI_AVEncCommonRateControlMode,
                    &ui4_variant(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
                );
                // With no explicit peak, vendor MFTs typically clamp bursts
                // near the mean, which starves hard frames (motion, scene
                // cuts) and reads as blocking. 2x the mean lets VBR absorb
                // them while the controller still owns the average.
                let _ = codec.SetValue(
                    &CODECAPI_AVEncCommonMaxBitRate,
                    &ui4_variant(self.bitrate_bps.saturating_mul(2)),
                );
                // Quality-vs-speed bias (0 = best quality, 100 = fastest).
                // v4 defaults to responsiveness; adaptive quality must not reintroduce latency.
                let quality_speed: u32 = 70;
                let _ = codec.SetValue(
                    &CODECAPI_AVEncCommonQualityVsSpeed,
                    &ui4_variant(quality_speed),
                );
                if self.codec == VideoCodec::H264 {
                    let _ = codec.SetValue(&CODECAPI_AVEncH264CABACEnable, &low_latency);
                }
            }
        }
        let events: IMFMediaEventGenerator = transform
            .cast()
            .context("hardware MFT has no asynchronous event interface")?;
        let (subtype, profile) = output_format(self.codec, true);
        let output = create_video_type(
            &subtype,
            width,
            height,
            self.max_fps,
            Some(self.bitrate_bps.saturating_mul(1)),
        )?;
        // SAFETY: output-before-input is required by the Microsoft H.264 encoder contract.
        unsafe {
            output
                .SetUINT32(&MF_MT_COMPRESSED, 1)
                .context("cannot mark encoder output as compressed")?;
            output
                .SetUINT32(&MF_MT_MPEG2_PROFILE, profile)
                .context("cannot request encoder profile")?;
            transform
                .SetOutputType(0, &output, 0)
                .context("hardware MFT rejected encoder output settings")?;
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
        let (activation, transform) = activated.into_parts();
        let mut active = ActiveEncoder {
            activation,
            transform,
            events,
            width,
            height,
            output_buffer_size,
            provides_output_samples,
            needs_input: false,
            first_access_unit: true,
            shut_down: false,
        };
        wait_until_input_needed(&mut active, Instant::now() + MFT_STARTUP_BUDGET)?;
        Ok(active)
    }
}

/// Thread-owning Windows Media Foundation encoder.
///
/// Vendor MFT COM objects are not safe to move between arbitrary Tokio worker threads.
/// This public proxy is the `Send` pipeline object; the actual encoder is created, used,
/// and dropped on one dedicated blocking thread. At most one encode is outstanding because
/// the trait call awaits that encode's response before it can be called again.
pub struct WindowsEncoder {
    request_tx: Option<mpsc::SyncSender<EncoderJob>>,
    response_rx: tokio::sync::mpsc::Receiver<(u64, Result<EncodedBitstream, String>)>,
    thread: Option<JoinHandle<()>>,
    next_ticket: u64,
}

struct EncoderJob {
    ticket: u64,
    request: EncodeRequest,
}

impl WindowsEncoder {
    /// Creates a hardware-only H.264/HEVC encoder. `bitrate_bps` is bits per second.
    ///
    /// AV1 is rejected explicitly until its bitstream validator lands; callers must not
    /// silently treat that unsupported codec as H.264.
    pub fn new(
        codec: VideoCodec,
        bitrate_bps: u32,
        frame_rate: u32,
        dimensions: (u32, u32),
    ) -> Result<Self, MediaError> {
        if codec == VideoCodec::Av1 {
            return Err(MediaError::InvalidConfig(
                "AV1 Media Foundation encoding is not implemented yet",
            ));
        }
        if bitrate_bps == 0 {
            return Err(MediaError::InvalidConfig("target bitrate must be nonzero"));
        }
        if !(1..=240).contains(&frame_rate) {
            return Err(MediaError::InvalidConfig(
                "frame rate must be between 1 and 240",
            ));
        }
        if dimensions.0 == 0
            || dimensions.1 == 0
            || !dimensions.0.is_multiple_of(2)
            || !dimensions.1.is_multiple_of(2)
        {
            return Err(MediaError::InvalidConfig(
                "encoder dimensions must be non-zero and even",
            ));
        }
        let max_fps = frame_rate as u16;
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (request_tx, request_rx) = mpsc::sync_channel::<EncoderJob>(1);
        let (response_tx, response_rx) = tokio::sync::mpsc::channel(1);
        let thread = std::thread::spawn(move || {
            encoder_thread(
                codec,
                bitrate_bps,
                max_fps,
                dimensions,
                ready_tx,
                request_rx,
                response_tx,
            );
        });
        ready_rx
            .recv()
            .map_err(|_| MediaError::Encode("encoder worker exited before startup".to_string()))?
            .map_err(MediaError::Encode)?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            thread: Some(thread),
            next_ticket: 0,
        })
    }
}

impl Drop for WindowsEncoder {
    fn drop(&mut self) {
        // A cancelled encode can leave a stale response queued. Closing the receiver makes a
        // worker blocked in `blocking_send` fail instead of deadlocking the join below.
        self.response_rx.close();
        self.request_tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[async_trait]
impl VideoEncoder for WindowsEncoder {
    async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
        let Some(request_tx) = &self.request_tx else {
            return Err(MediaError::Encode("encoder is shut down".to_string()));
        };
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        request_tx
            .try_send(EncoderJob { ticket, request })
            .map_err(|_| MediaError::Encode("encoder worker is busy or stopped".to_string()))?;
        loop {
            let (response_ticket, response) = self
                .response_rx
                .recv()
                .await
                .ok_or_else(|| MediaError::Encode("encoder worker exited".to_string()))?;
            if response_ticket == ticket {
                return response.map_err(MediaError::Encode);
            }
        }
    }
}

fn encoder_thread(
    codec: VideoCodec,
    bitrate_bps: u32,
    max_fps: u16,
    dimensions: (u32, u32),
    ready_tx: mpsc::Sender<Result<(), String>>,
    request_rx: mpsc::Receiver<EncoderJob>,
    response_tx: tokio::sync::mpsc::Sender<(u64, Result<EncodedBitstream, String>)>,
) {
    let mut encoder = match MftEncoder::new(codec, bitrate_bps, max_fps, dimensions) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("{error:#}")));
            return;
        }
    };
    if ready_tx.send(Ok(())).is_err() {
        return;
    }
    while let Ok(job) = request_rx.recv() {
        let result = encoder
            .encode_blocking(&job.request)
            .map_err(|error| format!("{error:#}"));
        if response_tx.blocking_send((job.ticket, result)).is_err() {
            return;
        }
    }
}

fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Av1 => "AV1",
    }
}

fn output_format(codec: VideoCodec, h264_high: bool) -> (windows::core::GUID, u32) {
    match codec {
        VideoCodec::H264 if h264_high => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
        VideoCodec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_Base.0 as u32),
        VideoCodec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
        VideoCodec::Av1 => (MFVideoFormat_AV1, eAVEncAV1VProfile_Main_420_8.0 as u32),
    }
}

fn sequence_header(codec: VideoCodec, transform: &IMFTransform) -> Option<Vec<u8>> {
    // SAFETY: stream zero owns the output media type; GetBlob writes into the provided slice.
    unsafe {
        let media_type = transform.GetOutputCurrentType(0).ok()?;
        let size = media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()? as usize;
        if size == 0 || size > 64 * 1024 {
            return None;
        }
        let mut bytes = vec![0; size];
        media_type
            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut bytes, None)
            .ok()?;
        has_parameter_sets(codec, &bytes).then_some(bytes)
    }
}

fn ui4_variant(value: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: value },
            }),
        },
    }
}

fn enumerate_hardware_encoders(codec: VideoCodec) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: output_format(codec, false).0,
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
        bail!(
            "no NV12-to-{} hardware MFT is installed",
            codec_label(codec)
        );
    }
    if count == 0 {
        unsafe {
            CoTaskMemFree(Some(activations.cast()));
        }
        bail!(
            "no NV12-to-{} hardware MFT is installed",
            codec_label(codec)
        );
    }
    // SAFETY: the successful call above returned exactly `count` slots. Taking each Option moves
    // ownership of its COM reference before the backing pointer array is freed.
    let available = unsafe { slice::from_raw_parts_mut(activations, count as usize) };
    let owned: Vec<IMFActivate> = available.iter_mut().filter_map(Option::take).collect();
    unsafe {
        CoTaskMemFree(Some(activations.cast()));
    }
    Ok(owned)
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

fn create_input_sample(
    byte_len: usize,
    timestamp: i64,
    duration: i64,
    fill: impl FnOnce(&mut [u8]) -> Result<()>,
) -> Result<IMFSample> {
    let length = u32::try_from(byte_len).context("NV12 frame is too large")?;
    // SAFETY: the locked buffer is valid for `length` bytes and is always unlocked before return.
    unsafe {
        let buffer =
            MFCreateMemoryBuffer(length).context("cannot allocate an NV12 media buffer")?;
        let mut destination = ptr::null_mut();
        buffer
            .Lock(&mut destination, None, None)
            .context("cannot lock an NV12 media buffer")?;
        let output = slice::from_raw_parts_mut(destination, byte_len);
        let fill_result = fill(output);
        let unlock_result = buffer.Unlock();
        fill_result?;
        unlock_result.context("cannot unlock an NV12 media buffer")?;
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

fn wait_until_input_needed(active: &mut ActiveEncoder, deadline: Instant) -> Result<()> {
    if active.needs_input {
        return Ok(());
    }
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

fn wait_for_output(active: &mut ActiveEncoder, deadline: Instant) -> Result<Vec<u8>> {
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
    if is_annex_b(bytes) {
        Ok(())
    } else {
        bail!("hardware MFT produced output without Annex-B start codes")
    }
}

#[cfg(test)]
mod tests {
    use nanoctl_core::video::VideoCodec;

    use super::{WindowsEncoder, validate_annex_b};
    use crate::bitstream::has_parameter_sets;
    use crate::types::{EncodeRequest, PixelFormat, RawFrame, VideoEncoder};

    #[test]
    fn validates_only_annex_b_access_units() {
        assert!(validate_annex_b(&[0, 0, 0, 1, 0x67]).is_ok());
        assert!(validate_annex_b(&[0, 0, 1, 0x65]).is_ok());
        assert!(validate_annex_b(&[0, 0, 0, 2, 0x65]).is_err());
    }

    #[test]
    fn rejects_invalid_encoder_configuration_before_spawning() {
        assert!(WindowsEncoder::new(VideoCodec::H264, 0, 60, (1280, 720)).is_err());
        assert!(WindowsEncoder::new(VideoCodec::H264, 1_000_000, 0, (1280, 720)).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    #[ignore = "requires Windows hardware Media Foundation encoder"]
    async fn encodes_real_h264_keyframe() {
        let mut encoder =
            WindowsEncoder::new(VideoCodec::H264, 2_000_000, 60, (1280, 720)).unwrap();
        let frame = RawFrame::new(0, 1280, 720, PixelFormat::Rgba32, vec![128; 1280 * 720 * 4]);
        let encoded = encoder
            .encode(EncodeRequest {
                stream_sequence: 1,
                force_keyframe: true,
                target_bitrate: 2_000_000,
                codec: VideoCodec::H264,
                frame,
            })
            .await
            .unwrap();
        assert!(encoded.keyframe);
        assert!(validate_annex_b(&encoded.payload).is_ok());
        assert!(has_parameter_sets(VideoCodec::H264, &encoded.payload));
    }
}
