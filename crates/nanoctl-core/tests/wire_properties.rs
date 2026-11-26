use nanoctl_core::input::{
    HostAction, InputMessage, InputStateMachine, KeyIdentity, NormalizedPoint,
};
use nanoctl_core::video::{VideoChunk, VideoControlMessage};

#[test]
fn mutated_wire_messages_reject_or_roundtrip() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../test-vectors/wire-v4.json")).unwrap();
    let mut seed = 0xa175_3a91_u32;
    for vector in fixture["vectors"].as_array().unwrap() {
        let hex = vector["hex"].as_str().unwrap();
        let original: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap())
            .collect();
        for bytes in (0..=original.len())
            .map(|len| original[..len].to_vec())
            .chain((0..128).map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut bytes = original.clone();
                if !bytes.is_empty() {
                    let offset = (seed >> 8) as usize % bytes.len();
                    bytes[offset] ^= (seed >> 24) as u8;
                }
                bytes
            }))
        {
            if vector["lane"] == "input" {
                if let Ok(message) = InputMessage::parse(&bytes) {
                    assert_eq!(InputMessage::parse(&message.encode().unwrap()), Ok(message));
                }
            } else if let Ok(chunk) = VideoChunk::parse(&bytes) {
                assert_eq!(VideoChunk::parse(&chunk.encode().unwrap()), Ok(chunk));
            }
        }
    }
}

#[test]
fn keyframe_requests_accept_exactly_one_two_byte_encoding() {
    for version in u8::MIN..=u8::MAX {
        for kind in u8::MIN..=u8::MAX {
            assert_eq!(
                VideoControlMessage::parse(&[version, kind]).is_ok(),
                version == 4 && kind == 0x41,
            );
        }
    }
}

#[test]
fn duplicate_and_stale_motion_cannot_move_pointer_backwards_or_keep_keys_held() {
    for start in [0, 1, u32::MAX - 1, u32::MAX] {
        let mut input = InputStateMachine::new();
        let identity = KeyIdentity {
            code: "KeyA".into(),
            key: "a".into(),
        };
        let key = InputMessage::Key {
            sequence: 0,
            down: true,
            browser_repeat: false,
            identity: identity.clone(),
        };
        assert_eq!(
            input.apply(&key, 0),
            [HostAction::KeyDown(identity.clone())]
        );
        for index in 0..128_u32 {
            let point = NormalizedPoint {
                x: index as u16,
                y: 5,
            };
            let motion = InputMessage::PointerMove {
                sequence: start.wrapping_add(index),
                point,
            };
            let stale = InputMessage::PointerMove {
                sequence: start.wrapping_add(index).wrapping_sub(1),
                point: NormalizedPoint {
                    x: u16::MAX,
                    y: u16::MAX,
                },
            };
            let now = u64::from(index) + 1;
            assert_eq!(input.apply(&motion, now), [HostAction::PointerMove(point)]);
            assert!(input.apply(&motion, now).is_empty());
            assert!(input.apply(&stale, now).is_empty());
        }
        assert_eq!(input.tick(10_000), [HostAction::KeyUp(identity)]);
        assert!(input.tick(20_000).is_empty());
    }
}
