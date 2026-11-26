use async_trait::async_trait;
use nanoctl_core::video::VideoCodec;

use crate::openh264_encoder::OpenH264Encoder;
use crate::types::{EncodeRequest, EncodedBitstream, MediaError, VideoEncoder};
#[cfg(target_os = "windows")]
use crate::windows_mft::WindowsEncoder;

/// Host encoder policy. `Auto` uses hardware when it is real and usable, but keeps
/// software H.264 as a correctness fallback; `Hardware` is strict and must fail rather
/// than silently becoming software.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderSelection {
    Auto,
    Hardware,
    Software,
}

enum ActiveEncoder {
    #[cfg(target_os = "windows")]
    Hardware(WindowsEncoder),
    Software(Box<OpenH264Encoder>),
}

impl ActiveEncoder {
    fn backend_name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "windows")]
            Self::Hardware(_) => "windows-mft-hardware",
            Self::Software(_) => "software-openh264",
        }
    }
}

#[async_trait]
impl VideoEncoder for ActiveEncoder {
    async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
        match self {
            #[cfg(target_os = "windows")]
            Self::Hardware(encoder) => encoder.encode(request).await,
            Self::Software(encoder) => encoder.encode(request).await,
        }
    }
}

/// Dimension-lazy platform encoder. Windows hardware MFTs need real dimensions at
/// activation time, but dimensions are known only after the first scaled capture frame.
pub struct PlatformEncoder {
    selection: EncoderSelection,
    bitrate_bps: u32,
    frame_rate: u32,
    codec: VideoCodec,
    active: Option<ActiveEncoder>,
}

impl PlatformEncoder {
    pub fn new(
        selection: EncoderSelection,
        bitrate_bps: u32,
        frame_rate: u32,
        codec: VideoCodec,
    ) -> Result<Self, MediaError> {
        if bitrate_bps == 0 {
            return Err(MediaError::InvalidConfig("target bitrate must be nonzero"));
        }
        if !(1..=240).contains(&frame_rate) {
            return Err(MediaError::InvalidConfig(
                "frame rate must be between 1 and 240",
            ));
        }
        Ok(Self {
            selection,
            bitrate_bps,
            frame_rate,
            codec,
            active: None,
        })
    }

    fn create_active(
        &self,
        dimensions: (u32, u32),
        hardware_error: &mut Option<MediaError>,
    ) -> Result<ActiveEncoder, MediaError> {
        #[cfg(not(target_os = "windows"))]
        let _ = dimensions;
        #[cfg(target_os = "windows")]
        if !matches!(self.selection, EncoderSelection::Software) {
            match WindowsEncoder::new(self.codec, self.bitrate_bps, self.frame_rate, dimensions) {
                Ok(encoder) => return Ok(ActiveEncoder::Hardware(encoder)),
                Err(error) => {
                    if matches!(self.selection, EncoderSelection::Hardware) {
                        return Err(error);
                    }
                    *hardware_error = Some(error);
                }
            }
        }

        if matches!(self.selection, EncoderSelection::Hardware) {
            return Err(hardware_error.take().unwrap_or(MediaError::Encode(
                "no hardware encoder backend exists on this platform".to_string(),
            )));
        }
        if self.codec != VideoCodec::H264 {
            return Err(hardware_error.take().unwrap_or(MediaError::Encode(
                "software fallback is only available for H264".to_string(),
            )));
        }
        Ok(ActiveEncoder::Software(Box::new(OpenH264Encoder::new(
            self.bitrate_bps,
            self.frame_rate,
        )?)))
    }

    fn active_is_hardware(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            matches!(self.active.as_ref(), Some(ActiveEncoder::Hardware(_)))
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    fn replace_with_software(&mut self) -> Result<(), MediaError> {
        if self.codec != VideoCodec::H264 {
            return Err(MediaError::Encode(
                "software fallback is only available for H264".to_string(),
            ));
        }
        self.active = Some(ActiveEncoder::Software(Box::new(OpenH264Encoder::new(
            self.bitrate_bps,
            self.frame_rate,
        )?)));
        Ok(())
    }
}

#[async_trait]
impl VideoEncoder for PlatformEncoder {
    async fn encode(&mut self, request: EncodeRequest) -> Result<EncodedBitstream, MediaError> {
        if self.active.is_none() {
            let mut hardware_error = None;
            let active = self.create_active(
                (request.frame.width, request.frame.height),
                &mut hardware_error,
            )?;
            if let Some(error) = hardware_error {
                tracing::warn!(%error, "hardware encoder unavailable; using software fallback");
            }
            let backend = active.backend_name();
            self.active = Some(active);
            tracing::info!(backend, "v4 platform encoder initialized");
        }

        let retry = matches!(self.selection, EncoderSelection::Auto)
            && self.active_is_hardware()
            && self.codec == VideoCodec::H264;
        let retry_request = retry.then(|| EncodeRequest {
            force_keyframe: true,
            target_bitrate: request.target_bitrate,
            codec: request.codec,
            frame: request.frame.clone(),
            stream_sequence: request.stream_sequence,
        });
        let result = self
            .active
            .as_mut()
            .expect("platform encoder is initialized")
            .encode(request)
            .await;
        match (result, retry_request) {
            (Ok(encoded), _) => Ok(encoded),
            (Err(error), None) => Err(error),
            (Err(hardware_error), Some(request)) => {
                tracing::warn!(%hardware_error, "hardware encode failed; switching to software");
                self.replace_with_software()?;
                self.active
                    .as_mut()
                    .expect("software fallback is initialized")
                    .encode(request)
                    .await
                    .map_err(|software_error| {
                        MediaError::Encode(format!(
                            "hardware encode failed ({hardware_error}); software fallback failed ({software_error})"
                        ))
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EncoderSelection, PlatformEncoder};
    use crate::types::{PixelFormat, RawFrame, VideoEncoder};
    use nanoctl_core::video::VideoCodec;

    fn request(force_keyframe: bool) -> crate::types::EncodeRequest {
        crate::types::EncodeRequest {
            stream_sequence: 0,
            force_keyframe,
            target_bitrate: 2_000_000,
            codec: VideoCodec::H264,
            frame: RawFrame::new(0, 64, 64, PixelFormat::Bgra32, vec![128; 64 * 64 * 4]),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn auto_selection_encodes_h264_with_hardware_or_software_fallback() {
        let mut encoder =
            PlatformEncoder::new(EncoderSelection::Auto, 2_000_000, 60, VideoCodec::H264).unwrap();
        let encoded = encoder.encode(request(true)).await.unwrap();
        assert!(encoded.keyframe);
        assert!(!encoded.payload.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn explicit_software_selection_encodes_h264() {
        let mut encoder =
            PlatformEncoder::new(EncoderSelection::Software, 2_000_000, 60, VideoCodec::H264)
                .unwrap();
        let encoded = encoder.encode(request(true)).await.unwrap();
        assert!(encoded.keyframe);
        assert!(!encoded.payload.is_empty());
    }

    #[test]
    fn rejects_invalid_shared_encoder_configuration() {
        assert!(PlatformEncoder::new(EncoderSelection::Auto, 0, 60, VideoCodec::H264).is_err());
        assert!(
            PlatformEncoder::new(EncoderSelection::Auto, 2_000_000, 0, VideoCodec::H264).is_err()
        );
    }
}
