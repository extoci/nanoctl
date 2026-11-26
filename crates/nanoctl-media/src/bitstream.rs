use nanoctl_core::video::VideoCodec;

pub(crate) fn is_annex_b(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1])
}

pub(crate) fn annex_b_nal_headers(bytes: &[u8]) -> Vec<u8> {
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
        offset += start_length;
        if let Some(header) = bytes.get(offset) {
            headers.push(*header);
            offset += 1;
        }
    }
    headers
}

pub(crate) fn is_keyframe(codec: VideoCodec, bytes: &[u8]) -> bool {
    let headers = annex_b_nal_headers(bytes);
    match codec {
        VideoCodec::H264 => headers.iter().any(|header| header & 0x1f == 5),
        VideoCodec::Hevc => headers
            .iter()
            .any(|header| (16..=21).contains(&((header >> 1) & 0x3f))),
        VideoCodec::Av1 => false,
    }
}

pub(crate) fn has_parameter_sets(codec: VideoCodec, bytes: &[u8]) -> bool {
    let headers = annex_b_nal_headers(bytes);
    match codec {
        VideoCodec::H264 => {
            let types: Vec<u8> = headers.iter().map(|header| header & 0x1f).collect();
            types.contains(&7) && types.contains(&8)
        }
        VideoCodec::Hevc => {
            let types: Vec<u8> = headers.iter().map(|header| (header >> 1) & 0x3f).collect();
            [32, 33, 34].iter().all(|nal| types.contains(nal))
        }
        VideoCodec::Av1 => false,
    }
}

#[cfg(test)]
mod tests {
    use nanoctl_core::video::VideoCodec;

    use super::{has_parameter_sets, is_annex_b, is_keyframe};

    #[test]
    fn validates_only_annex_b_access_units() {
        assert!(is_annex_b(&[0, 0, 0, 1, 0x67]));
        assert!(is_annex_b(&[0, 0, 1, 0x65]));
        assert!(!is_annex_b(&[0, 0, 0, 2, 0x65]));
    }

    #[test]
    fn detects_h264_keyframe_and_parameter_sets() {
        let unit = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1];
        assert!(is_keyframe(VideoCodec::H264, &unit));
        assert!(has_parameter_sets(VideoCodec::H264, &unit));
        assert!(!is_keyframe(VideoCodec::H264, &[0, 0, 1, 0x41, 1]));
        assert!(!has_parameter_sets(VideoCodec::H264, &unit[10..]));
    }

    #[test]
    fn detects_hevc_keyframe_and_parameter_sets() {
        let unit = [
            0, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1,
        ];
        assert!(is_keyframe(VideoCodec::Hevc, &unit));
        assert!(has_parameter_sets(VideoCodec::Hevc, &unit));
        assert!(!is_keyframe(VideoCodec::Hevc, &[0, 0, 1, 0x02, 1]));
    }
}
