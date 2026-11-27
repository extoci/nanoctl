use async_trait::async_trait;
use nanoctl_core::video::VideoCodec;
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, MatrixCoefficients,
    QpRange, RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVBuffer;
use openh264_sys2::{
    ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_0,
    SPATIAL_LAYER_ALL,
};

use crate::bitstream::{is_annex_b, is_keyframe};
use crate::i420::frame_to_i420;
use crate::types::{EncodeRequest, EncodedBitstream, MediaError, VideoEncoder};

/// Low-latency software H.264 encoder.
///
/// This is the correctness fallback for hosts without a hardware encoder. OpenH264 can only
/// enforce bitrate mode with frame skipping enabled, so skipped frames are represented by an
/// empty bitstream and surfaced to the pipeline as explicit drops rather than hidden loss.
pub struct OpenH264Encoder {
    encoder: Encoder,
    bitrate_bps: u32,
    frame_rate: u32,
    initialized: bool,
    dimensions: Option<(u32, u32)>,
}

impl OpenH264Encoder {
    pub fn new(bitrate_bps: u32, frame_rate: u32) -> Result<Self, MediaError> {
        validate_config(bitrate_bps, frame_rate)?;
        Ok(Self {
            encoder: create_encoder(bitrate_bps, frame_rate)?,
            bitrate_bps,
            frame_rate,
            initialized: false,
            dimensions: None,
        })
    }

    fn replace_uninitialized(&mut self, bitrate_bps: u32) -> Result<(), MediaError> {
        self.encoder = create_encoder(bitrate_bps, self.frame_rate)?;
        self.bitrate_bps = bitrate_bps;
        Ok(())
    }

    fn active_bitrate(&mut self) -> Result<i32, MediaError> {
        let mut current = SBitrateInfo {
            iLayer: SPATIAL_LAYER_0,
            iBitrate: 0,
        };
        // SAFETY: the initialized encoder writes to this matching option structure synchronously.
        let result = unsafe {
            self.encoder.raw_api().get_option(
                ENCODER_OPTION_BITRATE,
                std::ptr::from_mut(&mut current).cast(),
            )
        };
        if result != 0 {
            return Err(MediaError::Encode(format!(
                "OpenH264 active bitrate query failed: {result}"
            )));
        }
        Ok(current.iBitrate)
    }

    fn apply_bitrate(&mut self, target_bps: u32) -> Result<(), MediaError> {
        let target = SBitrateInfo {
            iLayer: SPATIAL_LAYER_ALL,
            iBitrate: target_bps.min(i32::MAX as u32) as i32,
        };
        let maximum = SBitrateInfo {
            iLayer: SPATIAL_LAYER_0,
            iBitrate: target_bps.saturating_mul(2).min(i32::MAX as u32) as i32,
        };
        // OpenH264 checks target <= maximum after each setter. A resize can also reset
        // the backend to its constructor bitrate, so read the active target first.
        let lowering = target.iBitrate < self.active_bitrate()?;
        let target = (ENCODER_OPTION_BITRATE, target, "target");
        let maximum = (ENCODER_OPTION_MAX_BITRATE, maximum, "maximum");
        let updates = if lowering {
            [target, maximum]
        } else {
            [maximum, target]
        };
        for (option, mut bitrate, name) in updates {
            // SAFETY: the initialized encoder consumes a matching, live option structure.
            let result = unsafe {
                self.encoder
                    .raw_api()
                    .set_option(option, std::ptr::from_mut(&mut bitrate).cast())
            };
            if result != 0 {
                return Err(MediaError::Encode(format!(
                    "OpenH264 {name} bitrate update to {} failed: {result}",
                    bitrate.iBitrate
                )));
            }
        }
        self.bitrate_bps = target_bps;
        Ok(())
    }

    fn encode_blocking(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
        if request.codec != VideoCodec::H264 {
            return Err(MediaError::EncoderContract(
                "OpenH264 encoder received a non-H264 request",
            ));
        }
        validate_frame(&request)?;
        if request.target_bitrate != self.bitrate_bps {
            if self.initialized {
                self.apply_bitrate(request.target_bitrate)?;
            } else {
                self.replace_uninitialized(request.target_bitrate)?;
            }
        }

        let dimensions = (request.frame.width, request.frame.height);
        let dimensions_changed = self.dimensions != Some(dimensions);
        let mut i420 = Vec::new();
        frame_to_i420(&request.frame, &mut i420)?;
        let yuv = YUVBuffer::from_vec(i420, dimensions.0 as usize, dimensions.1 as usize);
        if request.force_keyframe && self.initialized {
            self.encoder.force_intra_frame();
        }
        let bytes = self
            .encoder
            .encode(&yuv)
            .map_err(|error| MediaError::Encode(format!("OpenH264 encode failed: {error}")))?
            .to_vec();
        self.initialized = true;
        self.dimensions = Some(dimensions);
        if dimensions_changed {
            // OpenH264 reinitializes from its immutable constructor config after a resolution
            // change. Reapply the live bitrate so adaptive updates survive display handoffs.
            self.apply_bitrate(self.bitrate_bps)?;
        }
        if bytes.is_empty() {
            return Ok(EncodedBitstream {
                keyframe: false,
                payload: bytes,
            });
        }
        if !is_annex_b(&bytes) {
            return Err(MediaError::EncoderContract(
                "OpenH264 produced output without Annex-B start codes",
            ));
        }
        let keyframe = is_keyframe(VideoCodec::H264, &bytes);
        Ok(EncodedBitstream {
            keyframe,
            payload: bytes,
        })
    }
}

#[async_trait]
impl VideoEncoder for OpenH264Encoder {
    async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
        tokio::task::block_in_place(|| self.encode_blocking(request))
    }
}

fn validate_config(bitrate_bps: u32, frame_rate: u32) -> Result<(), MediaError> {
    if bitrate_bps == 0 {
        return Err(MediaError::InvalidConfig("target bitrate must be nonzero"));
    }
    if !(1..=240).contains(&frame_rate) {
        return Err(MediaError::InvalidConfig(
            "frame rate must be between 1 and 240",
        ));
    }
    Ok(())
}

fn validate_frame(request: &EncodeRequest) -> Result<(), MediaError> {
    let (width, height) = (request.frame.width, request.frame.height);
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(MediaError::Encode(
            "OpenH264 dimensions must be non-zero and even".to_string(),
        ));
    }
    if width.max(height) > 3840 || width.min(height) > 2160 {
        return Err(MediaError::Encode(
            "OpenH264 dimensions exceed its 3840x2160 limit".to_string(),
        ));
    }
    if request.target_bitrate == 0 {
        return Err(MediaError::Encode(
            "OpenH264 target bitrate must be nonzero".to_string(),
        ));
    }
    Ok(())
}

fn encode_threads() -> u16 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(4) as u16)
        .unwrap_or(2)
}

fn slice_len(bitrate_bps: u32, frame_rate: u32, threads: u16) -> u32 {
    let frame_bytes = u64::from(bitrate_bps) / 8 / u64::from(frame_rate.max(1));
    (frame_bytes / u64::from(threads.max(1))).clamp(2_048, 16_384) as u32
}

fn create_encoder(bitrate_bps: u32, frame_rate: u32) -> Result<Encoder, MediaError> {
    let threads = encode_threads();
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate_bps.min(i32::MAX as u32)))
        .max_frame_rate(FrameRate::from_hz(frame_rate as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(u32::MAX))
        .usage_type(UsageType::CameraVideoRealTime)
        .vui(
            VuiConfig::srgb()
                .full_range(false)
                .matrix_coefficients(MatrixCoefficients::Smpte170M),
        )
        .rate_control_mode(RateControlMode::Bitrate)
        // Low complexity is the responsiveness default. QP 26 avoids spending invisible bits on
        // static desktops while still allowing hard frames to use the full bitrate budget.
        .qp(QpRange::new(26, 51))
        .complexity(Complexity::Low)
        // OpenH264 can only enforce bitrate mode when frame skipping is enabled. The pipeline
        // treats empty results as explicit drops and immediately restarts the reference chain.
        .skip_frames(true)
        .scene_change_detect(false)
        .max_slice_len(slice_len(bitrate_bps, frame_rate, threads))
        .num_threads(threads);
    Encoder::with_api_config(OpenH264API::from_source(), config)
        .map_err(|error| MediaError::Encode(format!("OpenH264 encoder is unavailable: {error}")))
}

#[cfg(test)]
mod tests {
    use nanoctl_core::video::VideoCodec;

    use super::{OpenH264Encoder, slice_len};
    use crate::bitstream::has_parameter_sets;
    use crate::types::{EncodeRequest, PixelFormat, RawFrame, VideoEncoder};

    fn request(sequence: u32, force_keyframe: bool, width: u32, height: u32) -> EncodeRequest {
        EncodeRequest {
            stream_sequence: sequence,
            force_keyframe,
            target_bitrate: 2_000_000,
            codec: VideoCodec::H264,
            frame: RawFrame::new(
                0,
                width,
                height,
                PixelFormat::Rgba32,
                vec![128; width as usize * height as usize * 4],
            ),
        }
    }

    #[test]
    fn slice_len_targets_encode_threads_without_tiny_slices() {
        assert_eq!(slice_len(8_000_000, 60, 4), 4_166);
        assert_eq!(slice_len(24_000_000, 60, 4), 12_500);
        assert_eq!(slice_len(100_000, 60, 4), 2_048);
        assert_eq!(slice_len(900_000_000, 60, 1), 16_384);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn encodes_h264_keyframe_and_delta() {
        let mut encoder = OpenH264Encoder::new(2_000_000, 60).unwrap();
        let keyframe = encoder.encode(request(1, true, 128, 64)).await.unwrap();
        assert!(keyframe.keyframe);
        assert!(has_parameter_sets(VideoCodec::H264, &keyframe.payload));
        let delta = encoder.encode(request(2, false, 128, 64)).await.unwrap();
        assert!(!delta.payload.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn honors_forced_keyframes() {
        let mut encoder = OpenH264Encoder::new(2_000_000, 60).unwrap();
        let _ = encoder.encode(request(1, true, 128, 64)).await.unwrap();
        let _ = encoder.encode(request(2, false, 128, 64)).await.unwrap();
        let forced = encoder.encode(request(3, true, 128, 64)).await.unwrap();
        assert!(forced.keyframe);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn applies_sharp_bitrate_cuts_and_restores_them_after_resize() {
        let mut encoder = OpenH264Encoder::new(2_000_000, 60).unwrap();
        encoder.encode(request(1, true, 128, 64)).await.unwrap();

        let mut lower = request(2, false, 128, 64);
        lower.target_bitrate = 200_000;
        encoder.encode(lower).await.unwrap();
        assert_eq!(encoder.active_bitrate().unwrap(), 200_000);

        let mut resized = request(3, true, 160, 90);
        resized.target_bitrate = 200_000;
        assert!(encoder.encode(resized).await.unwrap().keyframe);
        assert_eq!(encoder.active_bitrate().unwrap(), 200_000);

        encoder.encode(request(4, false, 160, 90)).await.unwrap();
        assert_eq!(encoder.active_bitrate().unwrap(), 2_000_000);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn rejects_invalid_input_without_panicking() {
        let mut encoder = OpenH264Encoder::new(2_000_000, 60).unwrap();
        assert!(encoder.encode(request(1, true, 127, 64)).await.is_err());
    }
}
