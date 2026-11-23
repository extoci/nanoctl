//! Video codec identity, SDP negotiation and bitstream inspection shared by the WebRTC
//! answerer and the encoder pipeline.

use std::sync::RwLock;

use anyhow::{Result, bail};

use crate::config::CodecPreference;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    /// Highest compression efficiency first.
    pub const BY_EFFICIENCY: [Self; 3] = [Self::Av1, Self::Hevc, Self::H264];

    pub fn id(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }

    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        }
    }

    fn from_rtpmap_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("H264") {
            Some(Self::H264)
        } else if name.eq_ignore_ascii_case("H265") {
            Some(Self::Hevc)
        } else if name.eq_ignore_ascii_case("AV1") {
            Some(Self::Av1)
        } else {
            None
        }
    }
}

static HOST_CODECS: RwLock<Vec<VideoCodec>> = RwLock::new(Vec::new());
static HOST_PREFERENCE: RwLock<CodecPreference> = RwLock::new(CodecPreference::Auto);

/// The operator's `quality.codec` setting, applied to every negotiation.
pub fn set_host_preference(preference: CodecPreference) {
    if let Ok(mut current) = HOST_PREFERENCE.write() {
        *current = preference;
    }
}

pub fn host_preference() -> CodecPreference {
    HOST_PREFERENCE
        .read()
        .map_or(CodecPreference::Auto, |current| *current)
}

/// Codecs this host can currently encode, most efficient first. H.264 is always available
/// because a software encoder backs it; HEVC and AV1 are recorded only after a native hardware
/// encoder opened successfully, and are withdrawn again when that encoder fails mid-session.
pub fn host_codecs() -> Vec<VideoCodec> {
    let mut codecs = HOST_CODECS
        .read()
        .map(|codecs| codecs.clone())
        .unwrap_or_default();
    if !codecs.contains(&VideoCodec::H264) {
        codecs.push(VideoCodec::H264);
    }
    codecs.sort_by_key(|codec| VideoCodec::BY_EFFICIENCY.iter().position(|c| c == codec));
    codecs
}

#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn set_host_codecs(codecs: &[VideoCodec]) {
    if let Ok(mut current) = HOST_CODECS.write() {
        current.clear();
        current.extend_from_slice(codecs);
    }
}

#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn withdraw_host_codec(codec: VideoCodec) {
    if codec == VideoCodec::H264 {
        return;
    }
    if let Ok(mut current) = HOST_CODECS.write() {
        current.retain(|candidate| *candidate != codec);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedCodec {
    pub codec: VideoCodec,
    /// The browser's fmtp parameters for the selected payload type, when it sent any.
    pub offered_fmtp: String,
    /// The browser accepts H.264 High (profile_idc 100) for this payload, so hardware encoders
    /// can use CABAC and 8x8 transforms. Baseline output from software fallbacks still decodes.
    pub h264_high: bool,
}

/// Selects the sender codec from a browser offer. The browser lists payload types in its
/// preference order (the viewer reorders them with `setCodecPreferences`), so the first
/// offered codec this host can encode wins. An explicit host preference overrides that order
/// when the browser offers it, and H.264 remains the universal fallback.
pub fn negotiate(
    offer_sdp: &str,
    host: &[VideoCodec],
    preference: CodecPreference,
) -> NegotiatedCodec {
    let offered = offered_video_codecs(offer_sdp);
    let usable = |codec: VideoCodec| host.contains(&codec) || codec == VideoCodec::H264;
    let preferred = match preference {
        CodecPreference::Auto | CodecPreference::Vp9 => None,
        CodecPreference::H264 => Some(VideoCodec::H264),
        CodecPreference::Hevc => Some(VideoCodec::Hevc),
        CodecPreference::Av1 => Some(VideoCodec::Av1),
    };
    preferred
        .and_then(|wanted| offered.iter().find(|offer| offer.codec == wanted))
        .filter(|offer| usable(offer.codec))
        .or_else(|| offered.iter().find(|offer| usable(offer.codec)))
        .cloned()
        .unwrap_or_else(NegotiatedCodec::default_h264)
}

fn offered_video_codecs(sdp: &str) -> Vec<NegotiatedCodec> {
    let mut order = Vec::<&str>::new();
    let mut names = Vec::<(&str, VideoCodec)>::new();
    let mut fmtp = Vec::<(&str, &str)>::new();
    let mut in_video = false;
    for line in sdp.lines().map(str::trim) {
        if let Some(media) = line.strip_prefix("m=") {
            if in_video {
                break;
            }
            in_video = media.starts_with("video ");
            if in_video {
                order = media.split_whitespace().skip(3).collect();
            }
            continue;
        }
        if !in_video {
            continue;
        }
        if let Some(rtpmap) = line.strip_prefix("a=rtpmap:") {
            let mut parts = rtpmap.splitn(2, ' ');
            if let (Some(pt), Some(encoding)) = (parts.next(), parts.next())
                && let Some(codec) = encoding
                    .split('/')
                    .next()
                    .and_then(VideoCodec::from_rtpmap_name)
            {
                names.push((pt, codec));
            }
        } else if let Some(parameters) = line.strip_prefix("a=fmtp:") {
            let mut parts = parameters.splitn(2, ' ');
            if let (Some(pt), Some(value)) = (parts.next(), parts.next()) {
                fmtp.push((pt, value.trim()));
            }
        }
    }
    let mut offered = Vec::new();
    for pt in order {
        let Some(&(_, codec)) = names.iter().find(|(candidate, _)| *candidate == pt) else {
            continue;
        };
        let parameters = fmtp
            .iter()
            .find(|(candidate, _)| *candidate == pt)
            .map_or("", |(_, value)| value);
        // Every nanoctl H.264 encoder emits single-NAL/FU-A packetization-mode=1 streams.
        if codec == VideoCodec::H264 && !fmtp_has(parameters, "packetization-mode", "1") {
            continue;
        }
        // HEVC and AV1 encoders emit 8-bit 4:2:0 Main / profile 0 streams.
        if (codec == VideoCodec::Hevc && !fmtp_allows(parameters, "profile-id", "1"))
            || (codec == VideoCodec::Av1 && !fmtp_allows(parameters, "profile", "0"))
        {
            continue;
        }
        let h264_high = codec == VideoCodec::H264 && offers_h264_high(parameters);
        if let Some(existing) = offered
            .iter_mut()
            .find(|existing: &&mut NegotiatedCodec| existing.codec == codec)
        {
            // Prefer the browser's High payload over its Baseline ones for the same codec.
            if h264_high && !existing.h264_high {
                existing.offered_fmtp = parameters.to_owned();
                existing.h264_high = true;
            }
            continue;
        }
        offered.push(NegotiatedCodec {
            codec,
            offered_fmtp: parameters.to_owned(),
            h264_high,
        });
    }
    offered
}

impl NegotiatedCodec {
    fn default_h264() -> Self {
        Self {
            codec: VideoCodec::H264,
            offered_fmtp: String::new(),
            h264_high: false,
        }
    }

    /// The answer echoes the offered profile_idc/profile-iop bytes so webrtc-rs matches the
    /// payload, with level 5.2 for 4K. Baseline output is decodable under every H.264 profile.
    pub fn h264_profile_level_id(&self) -> String {
        fmtp_value(&self.offered_fmtp, "profile-level-id")
            .filter(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .map_or_else(|| "42e034".to_owned(), |value| format!("{}34", &value[..4]))
    }
}

fn fmtp_value<'a>(parameters: &'a str, key: &str) -> Option<&'a str> {
    parameters.split(';').find_map(|parameter| {
        let mut pair = parameter.splitn(2, '=');
        (pair.next().map(str::trim) == Some(key)).then(|| pair.next().map(str::trim))?
    })
}

fn offers_h264_high(parameters: &str) -> bool {
    fmtp_value(parameters, "profile-level-id")
        .is_some_and(|value| value.len() == 6 && value[..2].eq_ignore_ascii_case("64"))
}

/// A missing parameter means the codec's default, which is the value nanoctl emits.
fn fmtp_allows(parameters: &str, key: &str, value: &str) -> bool {
    let present = parameters
        .split(';')
        .any(|parameter| parameter.split('=').next().map(str::trim) == Some(key));
    !present || fmtp_has(parameters, key, value)
}

fn fmtp_has(parameters: &str, key: &str, value: &str) -> bool {
    parameters.split(';').any(|parameter| {
        parameter
            .split_once('=')
            .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case(key) && v.trim() == value)
    })
}

/// First header byte of every Annex-B NAL unit (H.264 and HEVC).
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn annex_b_nal_headers(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > 32 * 1024 * 1024 {
        bail!("access unit exceeds the size limit");
    }
    let mut headers = Vec::new();
    let mut offset = 0_usize;
    while offset + 3 < bytes.len() {
        let start_length = if bytes[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        let header = offset + start_length;
        let Some(&byte) = bytes.get(header) else {
            bail!("access unit ends after a start code");
        };
        headers.push(byte);
        offset = header + 1;
    }
    if headers.is_empty() {
        bail!("access unit contains no Annex-B NAL units");
    }
    Ok(headers)
}

const AV1_OBU_SEQUENCE_HEADER: u8 = 1;
const AV1_OBU_FRAME_HEADER: u8 = 3;
const AV1_OBU_FRAME: u8 = 6;

struct Av1Summary {
    sequence_header: bool,
    key_frame: bool,
    frames: usize,
}

fn av1_summary(bytes: &[u8]) -> Result<Av1Summary> {
    let mut summary = Av1Summary {
        sequence_header: false,
        key_frame: false,
        frames: 0,
    };
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let header = bytes[offset];
        if header & 0x80 != 0 {
            bail!("AV1 OBU forbidden bit is set");
        }
        let obu_type = (header >> 3) & 0x0f;
        let has_extension = header & 0x04 != 0;
        if header & 0x02 == 0 {
            bail!("AV1 OBU has no size field");
        }
        offset += 1 + usize::from(has_extension);
        let mut size = 0_usize;
        let mut shift = 0;
        loop {
            let Some(&byte) = bytes.get(offset) else {
                bail!("AV1 OBU size is truncated");
            };
            offset += 1;
            size |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 28 {
                bail!("AV1 OBU size is too large");
            }
        }
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| anyhow::anyhow!("AV1 OBU exceeds the temporal unit"))?;
        match obu_type {
            AV1_OBU_SEQUENCE_HEADER => summary.sequence_header = true,
            AV1_OBU_FRAME_HEADER | AV1_OBU_FRAME => {
                summary.frames += 1;
                // show_existing_frame = 0 followed by frame_type = KEY_FRAME (0).
                if bytes.get(offset).is_some_and(|first| first & 0xe0 == 0) {
                    summary.key_frame = true;
                }
            }
            _ => {}
        }
        offset = end;
    }
    Ok(summary)
}

/// Whether an encoded access unit can start decoding without earlier frames.
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn is_keyframe(codec: VideoCodec, bytes: &[u8]) -> bool {
    match codec {
        VideoCodec::H264 => annex_b_nal_headers(bytes)
            .is_ok_and(|headers| headers.iter().any(|header| header & 0x1f == 5)),
        VideoCodec::Hevc => annex_b_nal_headers(bytes).is_ok_and(|headers| {
            headers
                .iter()
                .any(|header| (16..=21).contains(&((header >> 1) & 0x3f)))
        }),
        VideoCodec::Av1 => {
            av1_summary(bytes).is_ok_and(|summary| summary.sequence_header && summary.key_frame)
        }
    }
}

/// Whether the stream configuration (SPS/PPS, VPS/SPS/PPS or AV1 sequence header) is present.
#[cfg_attr(not(all(feature = "media", windows)), allow(dead_code))]
pub fn has_parameter_sets(codec: VideoCodec, bytes: &[u8]) -> bool {
    match codec {
        VideoCodec::H264 => annex_b_nal_headers(bytes).is_ok_and(|headers| {
            let types: Vec<u8> = headers.iter().map(|header| header & 0x1f).collect();
            types.contains(&7) && types.contains(&8)
        }),
        VideoCodec::Hevc => annex_b_nal_headers(bytes).is_ok_and(|headers| {
            let types: Vec<u8> = headers.iter().map(|header| (header >> 1) & 0x3f).collect();
            [32, 33, 34].iter().all(|nal| types.contains(nal))
        }),
        VideoCodec::Av1 => av1_summary(bytes).is_ok_and(|summary| summary.sequence_header),
    }
}

/// Rejects an opening access unit that a fresh decoder could not start from.
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn validate_initial_access_unit(codec: VideoCodec, bytes: &[u8]) -> Result<()> {
    match codec {
        VideoCodec::H264 => {
            let types: Vec<u8> = annex_b_nal_headers(bytes)?
                .into_iter()
                .map(|header| header & 0x1f)
                .collect();
            if ![5, 7, 8].iter().all(|nal| types.contains(nal)) {
                bail!("first H.264 access unit did not contain IDR, SPS and PPS");
            }
        }
        VideoCodec::Hevc => {
            let types: Vec<u8> = annex_b_nal_headers(bytes)?
                .into_iter()
                .map(|header| (header >> 1) & 0x3f)
                .collect();
            if ![32, 33, 34].iter().all(|nal| types.contains(nal))
                || !types.iter().any(|nal| (16..=21).contains(nal))
            {
                bail!("first HEVC access unit did not contain IRAP, VPS, SPS and PPS");
            }
        }
        VideoCodec::Av1 => {
            let summary = av1_summary(bytes)?;
            if !summary.sequence_header || !summary.key_frame || summary.frames == 0 {
                bail!("first AV1 temporal unit did not contain a sequence header and key frame");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_offered_h264_high_payload() {
        let offer = "m=video 9 UDP/TLS/RTP/SAVPF 102 103 104\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
a=rtpmap:103 H264/90000\r\n\
a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=640c1f\r\n\
a=rtpmap:104 H264/90000\r\n\
a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f\r\n";
        let chosen = negotiate(offer, &[VideoCodec::H264], CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::H264);
        assert!(chosen.h264_high);
        assert!(chosen.offered_fmtp.contains("640c1f"));
        assert!(chosen.offered_fmtp.contains("packetization-mode=1"));
        assert_eq!(chosen.h264_profile_level_id(), "640c34");

        let baseline_only = "m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 packetization-mode=1;profile-level-id=42e01f\r\n";
        let baseline = negotiate(baseline_only, &[VideoCodec::H264], CodecPreference::Auto);
        assert!(!baseline.h264_high);
        assert_eq!(baseline.h264_profile_level_id(), "42e034");
        assert_eq!(
            NegotiatedCodec::default_h264().h264_profile_level_id(),
            "42e034"
        );
    }

    const CHROME_OFFER: &str = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97 102 103 45 49 35\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n\
a=rtpmap:102 H264/90000\r\n\
a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42001f\r\n\
a=rtpmap:103 H264/90000\r\n\
a=fmtp:103 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f\r\n\
a=rtpmap:45 AV1/90000\r\n\
a=fmtp:45 level-idx=5;profile=0;tier=0\r\n\
a=rtpmap:49 H265/90000\r\n\
a=fmtp:49 level-id=156;profile-id=1;tier-flag=0;tx-mode=SRST\r\n\
a=rtpmap:35 AV1/90000\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=rtpmap:111 H265/90000\r\n";

    const ALL: &[VideoCodec] = &[VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264];

    #[test]
    fn browser_order_wins_in_auto_mode() {
        let chosen = negotiate(CHROME_OFFER, ALL, CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::H264);
        assert!(chosen.offered_fmtp.contains("packetization-mode=1"));
        let reordered = CHROME_OFFER.replace("96 97 102 103 45 49 35", "45 49 103 96");
        let chosen = negotiate(&reordered, ALL, CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::Av1);
        assert_eq!(chosen.offered_fmtp, "level-idx=5;profile=0;tier=0");
    }

    #[test]
    fn unsupported_or_unoffered_codecs_fall_back() {
        let reordered = CHROME_OFFER.replace("96 97 102 103 45 49 35", "45 49 103");
        let chosen = negotiate(&reordered, &[VideoCodec::H264], CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::H264);
        let chosen = negotiate(&reordered, &[VideoCodec::Hevc], CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::Hevc);
        let chosen = negotiate(CHROME_OFFER, ALL, CodecPreference::Hevc);
        assert_eq!(chosen.codec, VideoCodec::Hevc);
        assert!(chosen.offered_fmtp.contains("profile-id=1"));
        let h264_only = "m=video 9 RTP 103\r\na=rtpmap:103 H264/90000\r\n\
a=fmtp:103 packetization-mode=1\r\n";
        let chosen = negotiate(h264_only, ALL, CodecPreference::Av1);
        assert_eq!(chosen.codec, VideoCodec::H264);
        assert_eq!(
            negotiate("", ALL, CodecPreference::Av1).codec,
            VideoCodec::H264
        );
    }

    #[test]
    fn high_bit_depth_profiles_are_skipped() {
        let offer = "m=video 9 RTP 50 51 52 53\r\n\
a=rtpmap:50 H265/90000\r\na=fmtp:50 level-id=156;profile-id=2;tier-flag=0\r\n\
a=rtpmap:51 AV1/90000\r\na=fmtp:51 level-idx=5;profile=1;tier=0\r\n\
a=rtpmap:52 AV1/90000\r\n\
a=rtpmap:53 H265/90000\r\na=fmtp:53 level-id=156;profile-id=1;tier-flag=0\r\n";
        let chosen = negotiate(offer, ALL, CodecPreference::Auto);
        assert_eq!(chosen.codec, VideoCodec::Av1);
        assert_eq!(chosen.offered_fmtp, "");
        let chosen = negotiate(offer, ALL, CodecPreference::Hevc);
        assert!(chosen.offered_fmtp.contains("profile-id=1"));
    }

    #[test]
    fn host_codec_registry_keeps_h264_and_orders_by_efficiency() {
        set_host_codecs(&[VideoCodec::H264, VideoCodec::Av1, VideoCodec::Hevc]);
        assert_eq!(host_codecs(), ALL);
        withdraw_host_codec(VideoCodec::Av1);
        withdraw_host_codec(VideoCodec::H264);
        assert_eq!(host_codecs(), [VideoCodec::Hevc, VideoCodec::H264]);
        set_host_codecs(&[]);
        assert_eq!(host_codecs(), [VideoCodec::H264]);
    }

    #[test]
    fn detects_keyframes_per_codec() {
        let h264 = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1];
        assert!(is_keyframe(VideoCodec::H264, &h264));
        assert!(validate_initial_access_unit(VideoCodec::H264, &h264).is_ok());
        assert!(has_parameter_sets(VideoCodec::H264, &h264));
        assert!(!has_parameter_sets(VideoCodec::H264, &h264[10..]));
        assert!(!is_keyframe(VideoCodec::H264, &[0, 0, 1, 0x41, 1]));

        let hevc = [
            0, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1,
        ];
        assert!(is_keyframe(VideoCodec::Hevc, &hevc));
        assert!(validate_initial_access_unit(VideoCodec::Hevc, &hevc).is_ok());
        assert!(has_parameter_sets(VideoCodec::Hevc, &hevc));
        assert!(!is_keyframe(VideoCodec::Hevc, &[0, 0, 1, 0x02, 1]));
        assert!(validate_initial_access_unit(VideoCodec::Hevc, &[0, 0, 1, 0x26, 1]).is_err());

        // Temporal delimiter, sequence header, then a key frame OBU.
        let av1_key = [0x12, 0x00, 0x0a, 0x02, 0xaa, 0xbb, 0x32, 0x02, 0x10, 0x00];
        assert!(is_keyframe(VideoCodec::Av1, &av1_key));
        assert!(validate_initial_access_unit(VideoCodec::Av1, &av1_key).is_ok());
        assert!(has_parameter_sets(VideoCodec::Av1, &av1_key));
        let av1_inter = [0x12, 0x00, 0x32, 0x02, 0x30, 0x00];
        assert!(!is_keyframe(VideoCodec::Av1, &av1_inter));
        assert!(validate_initial_access_unit(VideoCodec::Av1, &av1_inter).is_err());
        assert!(!is_keyframe(VideoCodec::Av1, &[0x32, 0x09, 0x10]));
    }
}
