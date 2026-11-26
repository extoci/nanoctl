use nanoctl_core::input::InputMessage;
use nanoctl_core::video::VideoChunk;
use nanoctl_core::{WIRE_VERSION, WireError};

#[test]
fn wire_v4_golden_vectors_match_reference_parsers() {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../test-vectors/wire-v4.json"))
            .expect("wire vector document is valid JSON");
    assert_eq!(document["version"], WIRE_VERSION);

    for vector in document["vectors"].as_array().expect("vectors array") {
        let name = vector["name"].as_str().expect("vector name");
        let bytes = decode_hex(vector["hex"].as_str().expect("hex"));
        let lane = vector["lane"].as_str().expect("lane");
        match vector["expect"].as_str().expect("expect") {
            "valid" => match lane {
                "input" => {
                    let parsed = InputMessage::parse(&bytes).unwrap_or_else(|error| {
                        panic!("{name}: input vector rejected: {error}");
                    });
                    assert_input_decoded(name, &parsed, &vector["decoded"]);
                    assert_eq!(
                        parsed.encode().expect("valid input re-encodes"),
                        bytes,
                        "{name}: input re-encode differs"
                    );
                }
                "video" => {
                    let parsed = VideoChunk::parse(&bytes).unwrap_or_else(|error| {
                        panic!("{name}: video vector rejected: {error}");
                    });
                    assert_video_decoded(name, &parsed, &vector["decoded"]);
                    assert_eq!(
                        parsed.encode().expect("valid video re-encodes"),
                        bytes,
                        "{name}: video re-encode differs"
                    );
                }
                other => panic!("{name}: unknown lane {other}"),
            },
            "error" => {
                let error = match lane {
                    "input" => InputMessage::parse(&bytes).err(),
                    "video" => VideoChunk::parse(&bytes).err(),
                    other => panic!("{name}: unknown lane {other}"),
                }
                .unwrap_or_else(|| panic!("{name}: invalid vector was accepted"));
                assert_eq!(
                    error_name(&error),
                    vector["error"].as_str().expect("error kind"),
                    "{name}: wrong error"
                );
            }
            other => panic!("{name}: unknown expectation {other}"),
        }
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "hex has odd length");
    (0..hex.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).expect("hex byte"))
        .collect()
}

fn error_name(error: &WireError) -> &'static str {
    match error {
        WireError::Empty => "empty",
        WireError::Truncated => "truncated",
        WireError::UnsupportedVersion(_) => "unsupportedVersion",
        WireError::UnknownKind(_) => "unknownKind",
        WireError::InvalidLength => "invalidLength",
        WireError::InvalidFlags => "invalidFlags",
        WireError::InvalidUtf8 => "invalidUtf8",
        WireError::InvalidValue(_) => "invalidValue",
    }
}

fn assert_input_decoded(name: &str, parsed: &InputMessage, decoded: &serde_json::Value) {
    match (decoded["kind"].as_str().expect("decoded kind"), parsed) {
        ("heartbeat", InputMessage::Heartbeat { sequence }) => {
            assert_eq!(
                *sequence,
                decoded["sequence"].as_u64().unwrap() as u32,
                "{name}"
            );
        }
        ("pointerMove", InputMessage::PointerMove { sequence, point }) => {
            assert_eq!(
                *sequence,
                decoded["sequence"].as_u64().unwrap() as u32,
                "{name}"
            );
            assert_eq!(point.x, decoded["x"].as_u64().unwrap() as u16, "{name}");
            assert_eq!(point.y, decoded["y"].as_u64().unwrap() as u16, "{name}");
        }
        (
            "key",
            InputMessage::Key {
                sequence,
                down,
                browser_repeat,
                identity,
            },
        ) => {
            assert_eq!(
                *sequence,
                decoded["sequence"].as_u64().unwrap() as u32,
                "{name}"
            );
            assert_eq!(*down, decoded["down"].as_bool().unwrap(), "{name}");
            assert_eq!(
                *browser_repeat,
                decoded["browserRepeat"].as_bool().unwrap(),
                "{name}"
            );
            assert_eq!(identity.code, decoded["code"].as_str().unwrap(), "{name}");
            assert_eq!(identity.key, decoded["key"].as_str().unwrap(), "{name}");
        }
        (kind, message) => {
            panic!("{name}: decoded kind {kind} does not match parser result {message:?}")
        }
    }
}

fn assert_video_decoded(name: &str, parsed: &VideoChunk, decoded: &serde_json::Value) {
    assert_eq!(decoded["kind"].as_str().unwrap(), "videoChunk", "{name}");
    assert_eq!(
        parsed.stream_sequence,
        decoded["streamSequence"].as_u64().unwrap() as u32,
        "{name}"
    );
    assert_eq!(
        parsed.part,
        decoded["part"].as_u64().unwrap() as u16,
        "{name}"
    );
    assert_eq!(
        parsed.parts,
        decoded["parts"].as_u64().unwrap() as u16,
        "{name}"
    );
    assert_eq!(
        parsed.keyframe,
        decoded["keyframe"].as_bool().unwrap(),
        "{name}"
    );
    assert_eq!(
        parsed.capture_time_us,
        decoded["captureTimeUs"].as_u64().unwrap(),
        "{name}"
    );
}
