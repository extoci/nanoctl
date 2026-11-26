use crate::{
    WIRE_VERSION, WireError, f32_at, push_f32, push_u16, push_u32, u16_at, u32_at, versioned,
};
use std::collections::{BTreeMap, BTreeSet};

const KIND_POINTER_MOVE: u8 = 0x01;
const KIND_POINTER_BUTTON: u8 = 0x02;
const KIND_POINTER_WHEEL: u8 = 0x03;
const KIND_KEY: u8 = 0x04;
const KIND_TEXT: u8 = 0x05;
const KIND_RELEASE_ALL: u8 = 0x06;
const KIND_HEARTBEAT: u8 = 0x07;
const KIND_GAMEPAD_SNAPSHOT: u8 = 0x08;

const KEY_REPEAT_DELAY_MS: u64 = 500;
const KEY_REPEAT_INTERVAL_MS: u64 = 33;
pub const INPUT_LIVENESS_TIMEOUT_MS: u64 = 2_000;
pub const MAX_KEY_FIELD_BYTES: usize = 64;
pub const MAX_TEXT_BYTES: usize = 1_024;
pub const MAX_GAMEPADS: usize = 4;
const GAMEPAD_ENTRY_BYTES: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

impl PointerButton {
    fn from_wire(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Left),
            1 => Ok(Self::Middle),
            2 => Ok(Self::Right),
            _ => Err(WireError::InvalidValue("unknown pointer button")),
        }
    }

    fn wire(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyIdentity {
    pub code: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControllerKind {
    Xbox360,
    DualShock4,
    DualSense,
}

impl ControllerKind {
    fn from_wire(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Xbox360),
            1 => Ok(Self::DualShock4),
            2 => Ok(Self::DualSense),
            _ => Err(WireError::InvalidValue("unknown controller kind")),
        }
    }

    fn wire(self) -> u8 {
        match self {
            Self::Xbox360 => 0,
            Self::DualShock4 => 1,
            Self::DualSense => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GamepadState {
    pub slot: u8,
    pub kind: ControllerKind,
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GamepadSnapshot {
    pub pads: Vec<GamepadState>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputMessage {
    PointerMove {
        sequence: u32,
        point: NormalizedPoint,
    },
    PointerButton {
        sequence: u32,
        down: bool,
        button: PointerButton,
        point: NormalizedPoint,
    },
    PointerWheel {
        sequence: u32,
        point: NormalizedPoint,
        delta_x: f32,
        delta_y: f32,
    },
    Key {
        sequence: u32,
        down: bool,
        browser_repeat: bool,
        identity: KeyIdentity,
    },
    Text {
        sequence: u32,
        text: String,
    },
    ReleaseAll {
        sequence: u32,
    },
    Heartbeat {
        sequence: u32,
    },
    GamepadSnapshot {
        sequence: u32,
        snapshot: GamepadSnapshot,
    },
}

impl InputMessage {
    pub fn sequence(&self) -> u32 {
        match self {
            Self::PointerMove { sequence, .. }
            | Self::PointerButton { sequence, .. }
            | Self::PointerWheel { sequence, .. }
            | Self::Key { sequence, .. }
            | Self::Text { sequence, .. }
            | Self::ReleaseAll { sequence }
            | Self::Heartbeat { sequence }
            | Self::GamepadSnapshot { sequence, .. } => *sequence,
        }
    }

    fn validate(&self) -> Result<(), WireError> {
        match self {
            Self::PointerMove { .. }
            | Self::PointerButton { .. }
            | Self::ReleaseAll { .. }
            | Self::Heartbeat { .. } => Ok(()),
            Self::PointerWheel {
                delta_x, delta_y, ..
            } => {
                if delta_x.is_finite() && delta_y.is_finite() {
                    Ok(())
                } else {
                    Err(WireError::InvalidValue("non-finite wheel delta"))
                }
            }
            Self::Key { identity, .. } => {
                if identity.code.len() <= MAX_KEY_FIELD_BYTES
                    && !identity.key.is_empty()
                    && identity.key.len() <= MAX_KEY_FIELD_BYTES
                {
                    Ok(())
                } else {
                    Err(WireError::InvalidLength)
                }
            }
            Self::Text { text, .. } => {
                if !text.is_empty() && text.len() <= MAX_TEXT_BYTES {
                    Ok(())
                } else {
                    Err(WireError::InvalidLength)
                }
            }
            Self::GamepadSnapshot { snapshot, .. } => validate_gamepad_snapshot(snapshot),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.validate()?;
        let mut out = vec![WIRE_VERSION, 0];
        match self {
            Self::PointerMove { sequence, point } => {
                out[1] = KIND_POINTER_MOVE;
                push_u32(&mut out, *sequence);
                push_u16(&mut out, point.x);
                push_u16(&mut out, point.y);
            }
            Self::PointerButton {
                sequence,
                down,
                button,
                point,
            } => {
                out[1] = KIND_POINTER_BUTTON;
                push_u32(&mut out, *sequence);
                out.push(u8::from(*down));
                out.push(button.wire());
                push_u16(&mut out, point.x);
                push_u16(&mut out, point.y);
            }
            Self::PointerWheel {
                sequence,
                point,
                delta_x,
                delta_y,
            } => {
                out[1] = KIND_POINTER_WHEEL;
                push_u32(&mut out, *sequence);
                push_u16(&mut out, point.x);
                push_u16(&mut out, point.y);
                push_f32(&mut out, *delta_x);
                push_f32(&mut out, *delta_y);
            }
            Self::Key {
                sequence,
                down,
                browser_repeat,
                identity,
            } => {
                out[1] = KIND_KEY;
                push_u32(&mut out, *sequence);
                out.push((u8::from(!*down)) | (u8::from(*browser_repeat) << 1));
                out.push(identity.code.len() as u8);
                out.push(identity.key.len() as u8);
                out.extend_from_slice(identity.code.as_bytes());
                out.extend_from_slice(identity.key.as_bytes());
            }
            Self::Text { sequence, text } => {
                out[1] = KIND_TEXT;
                push_u32(&mut out, *sequence);
                push_u16(&mut out, text.len() as u16);
                out.extend_from_slice(text.as_bytes());
            }
            Self::ReleaseAll { sequence } => {
                out[1] = KIND_RELEASE_ALL;
                push_u32(&mut out, *sequence);
            }
            Self::Heartbeat { sequence } => {
                out[1] = KIND_HEARTBEAT;
                push_u32(&mut out, *sequence);
            }
            Self::GamepadSnapshot { sequence, snapshot } => {
                out[1] = KIND_GAMEPAD_SNAPSHOT;
                push_u32(&mut out, *sequence);
                out.push(snapshot.pads.len() as u8);
                for pad in &snapshot.pads {
                    out.push(pad.slot);
                    out.push(pad.kind.wire());
                    push_u16(&mut out, pad.buttons);
                    out.push(pad.left_trigger);
                    out.push(pad.right_trigger);
                    push_u16(&mut out, pad.left_x as u16);
                    push_u16(&mut out, pad.left_y as u16);
                    push_u16(&mut out, pad.right_x as u16);
                    push_u16(&mut out, pad.right_y as u16);
                }
            }
        }
        Ok(out)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, WireError> {
        let kind = versioned(bytes, KIND_POINTER_MOVE..=KIND_GAMEPAD_SNAPSHOT)?;
        let sequence = u32_at(bytes, 2)?;
        match kind {
            KIND_POINTER_MOVE => {
                if bytes.len() != 10 {
                    return Err(WireError::InvalidLength);
                }
                Ok(Self::PointerMove {
                    sequence,
                    point: point_at(bytes, 6)?,
                })
            }
            KIND_POINTER_BUTTON => {
                if bytes.len() != 12 {
                    return Err(WireError::InvalidLength);
                }
                let down = match bytes[6] {
                    0 => false,
                    1 => true,
                    _ => return Err(WireError::InvalidValue("pointer button action")),
                };
                Ok(Self::PointerButton {
                    sequence,
                    down,
                    button: PointerButton::from_wire(bytes[7])?,
                    point: point_at(bytes, 8)?,
                })
            }
            KIND_POINTER_WHEEL => {
                if bytes.len() != 18 {
                    return Err(WireError::InvalidLength);
                }
                let delta_x = f32_at(bytes, 10)?;
                let delta_y = f32_at(bytes, 14)?;
                if !delta_x.is_finite() || !delta_y.is_finite() {
                    return Err(WireError::InvalidValue("non-finite wheel delta"));
                }
                Ok(Self::PointerWheel {
                    sequence,
                    point: point_at(bytes, 6)?,
                    delta_x,
                    delta_y,
                })
            }
            KIND_KEY => {
                if bytes.len() < 9 {
                    return Err(WireError::Truncated);
                }
                let flags = bytes[6];
                if flags & !3 != 0 {
                    return Err(WireError::InvalidFlags);
                }
                let code_len = usize::from(bytes[7]);
                let key_len = usize::from(bytes[8]);
                if code_len > MAX_KEY_FIELD_BYTES
                    || key_len > MAX_KEY_FIELD_BYTES
                    || key_len == 0
                    || bytes.len() != 9 + code_len + key_len
                {
                    return Err(WireError::InvalidLength);
                }
                let code = utf8_at(bytes, 9, code_len)?;
                let key = utf8_at(bytes, 9 + code_len, key_len)?;
                Ok(Self::Key {
                    sequence,
                    down: flags & 1 == 0,
                    browser_repeat: flags & 2 != 0,
                    identity: KeyIdentity { code, key },
                })
            }
            KIND_TEXT => {
                if bytes.len() < 8 {
                    return Err(WireError::Truncated);
                }
                let len = usize::from(u16_at(bytes, 6)?);
                if len == 0 || len > MAX_TEXT_BYTES || bytes.len() != 8 + len {
                    return Err(WireError::InvalidLength);
                }
                Ok(Self::Text {
                    sequence,
                    text: utf8_at(bytes, 8, len)?,
                })
            }
            KIND_RELEASE_ALL => {
                if bytes.len() != 6 {
                    return Err(WireError::InvalidLength);
                }
                Ok(Self::ReleaseAll { sequence })
            }
            KIND_HEARTBEAT => {
                if bytes.len() != 6 {
                    return Err(WireError::InvalidLength);
                }
                Ok(Self::Heartbeat { sequence })
            }
            KIND_GAMEPAD_SNAPSHOT => {
                if bytes.len() < 7 {
                    return Err(WireError::Truncated);
                }
                let count = usize::from(bytes[6]);
                if count > MAX_GAMEPADS || bytes.len() != 7 + count * GAMEPAD_ENTRY_BYTES {
                    return Err(WireError::InvalidLength);
                }
                let mut slots = BTreeSet::new();
                let mut pads = Vec::with_capacity(count);
                for index in 0..count {
                    let offset = 7 + index * GAMEPAD_ENTRY_BYTES;
                    let slot = bytes[offset];
                    if slot >= MAX_GAMEPADS as u8 || !slots.insert(slot) {
                        return Err(WireError::InvalidValue(
                            "duplicate or out-of-range gamepad slot",
                        ));
                    }
                    pads.push(GamepadState {
                        slot,
                        kind: ControllerKind::from_wire(bytes[offset + 1])?,
                        buttons: u16_at(bytes, offset + 2)?,
                        left_trigger: bytes[offset + 4],
                        right_trigger: bytes[offset + 5],
                        left_x: u16_at(bytes, offset + 6)? as i16,
                        left_y: u16_at(bytes, offset + 8)? as i16,
                        right_x: u16_at(bytes, offset + 10)? as i16,
                        right_y: u16_at(bytes, offset + 12)? as i16,
                    });
                }
                Ok(Self::GamepadSnapshot {
                    sequence,
                    snapshot: GamepadSnapshot { pads },
                })
            }
            _ => unreachable!("namespace validation accepted unknown input kind"),
        }
    }
}

fn point_at(bytes: &[u8], offset: usize) -> Result<NormalizedPoint, WireError> {
    Ok(NormalizedPoint {
        x: u16_at(bytes, offset)?,
        y: u16_at(bytes, offset + 2)?,
    })
}

fn utf8_at(bytes: &[u8], offset: usize, len: usize) -> Result<String, WireError> {
    let value = bytes
        .get(offset..offset + len)
        .ok_or(WireError::Truncated)?;
    String::from_utf8(value.to_vec()).map_err(|_| WireError::InvalidUtf8)
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostAction {
    KeyDown(KeyIdentity),
    KeyRepeat(KeyIdentity),
    KeyUp(KeyIdentity),
    Text(String),
    PointerMove(NormalizedPoint),
    PointerButtonDown {
        button: PointerButton,
        point: NormalizedPoint,
    },
    PointerButtonUp {
        button: PointerButton,
        point: NormalizedPoint,
    },
    PointerWheel {
        point: NormalizedPoint,
        delta_x: f32,
        delta_y: f32,
    },
    GamepadSnapshot(GamepadSnapshot),
    NeutralizeGamepads,
}

#[derive(Debug, Clone)]
struct HeldKey {
    next_repeat_ms: u64,
    repeatable: bool,
}

#[derive(Debug, Default)]
pub struct InputStateMachine {
    held_keys: BTreeMap<KeyIdentity, HeldKey>,
    held_buttons: BTreeSet<PointerButton>,
    gamepads_present: bool,
    last_motion_sequence: Option<u32>,
    last_point: Option<NormalizedPoint>,
    last_seen_ms: Option<u64>,
    released_after_timeout: bool,
}

impl InputStateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, message: &InputMessage, now_ms: u64) -> Vec<HostAction> {
        self.last_seen_ms = Some(now_ms);
        self.released_after_timeout = false;
        match message {
            InputMessage::PointerMove { sequence, point } => {
                if self.is_stale_motion(*sequence) {
                    return Vec::new();
                }
                self.last_point = Some(*point);
                vec![HostAction::PointerMove(*point)]
            }
            InputMessage::PointerButton {
                down,
                button,
                point,
                ..
            } => {
                self.last_point = Some(*point);
                if *down {
                    if self.held_buttons.insert(*button) {
                        vec![HostAction::PointerButtonDown {
                            button: *button,
                            point: *point,
                        }]
                    } else {
                        Vec::new()
                    }
                } else if self.held_buttons.remove(button) {
                    vec![HostAction::PointerButtonUp {
                        button: *button,
                        point: *point,
                    }]
                } else {
                    Vec::new()
                }
            }
            InputMessage::PointerWheel {
                point,
                delta_x,
                delta_y,
                ..
            } => {
                self.last_point = Some(*point);
                vec![HostAction::PointerWheel {
                    point: *point,
                    delta_x: *delta_x,
                    delta_y: *delta_y,
                }]
            }
            InputMessage::Key { down, identity, .. } => {
                if *down {
                    if self.held_keys.contains_key(identity) {
                        return Vec::new();
                    }
                    let repeatable = is_typematic_key(identity);
                    self.held_keys.insert(
                        identity.clone(),
                        HeldKey {
                            next_repeat_ms: now_ms + KEY_REPEAT_DELAY_MS,
                            repeatable,
                        },
                    );
                    vec![HostAction::KeyDown(identity.clone())]
                } else if self.held_keys.remove(identity).is_some() {
                    vec![HostAction::KeyUp(identity.clone())]
                } else {
                    Vec::new()
                }
            }
            InputMessage::Text { text, .. } => vec![HostAction::Text(text.clone())],
            InputMessage::ReleaseAll { .. } => self.release_all_actions(),
            InputMessage::Heartbeat { .. } => Vec::new(),
            InputMessage::GamepadSnapshot { sequence, snapshot } => {
                if self.is_stale_motion(*sequence) {
                    return Vec::new();
                }
                self.gamepads_present = !snapshot.pads.is_empty();
                vec![HostAction::GamepadSnapshot(snapshot.clone())]
            }
        }
    }

    /// Advances host-side input state. At most one repeat per key is emitted per tick;
    /// delayed ticks skip missed repeat deadlines instead of producing catch-up bursts.
    pub fn tick(&mut self, now_ms: u64) -> Vec<HostAction> {
        if self.timed_out(now_ms) {
            return self.release_all_actions();
        }
        let mut actions = Vec::new();
        for (identity, held) in self.held_keys.iter_mut() {
            if held.repeatable && now_ms >= held.next_repeat_ms {
                actions.push(HostAction::KeyRepeat(identity.clone()));
                held.next_repeat_ms = now_ms + KEY_REPEAT_INTERVAL_MS;
            }
        }
        actions
    }

    fn timed_out(&self, now_ms: u64) -> bool {
        !self.released_after_timeout
            && self
                .last_seen_ms
                .is_some_and(|seen| now_ms.saturating_sub(seen) > INPUT_LIVENESS_TIMEOUT_MS)
    }

    fn is_stale_motion(&mut self, sequence: u32) -> bool {
        if self
            .last_motion_sequence
            .is_some_and(|last| !sequence_is_newer(sequence, last))
        {
            return true;
        }
        self.last_motion_sequence = Some(sequence);
        false
    }

    fn release_all_actions(&mut self) -> Vec<HostAction> {
        let mut actions = Vec::new();
        for identity in self.held_keys.keys().cloned().collect::<Vec<_>>() {
            actions.push(HostAction::KeyUp(identity));
        }
        self.held_keys.clear();
        for button in self.held_buttons.iter().copied().collect::<Vec<_>>() {
            actions.push(HostAction::PointerButtonUp {
                button,
                point: self.last_point.unwrap_or(NormalizedPoint { x: 0, y: 0 }),
            });
        }
        self.held_buttons.clear();
        if self.gamepads_present {
            actions.push(HostAction::NeutralizeGamepads);
            self.gamepads_present = false;
        }
        self.released_after_timeout = true;
        actions
    }
}

fn validate_gamepad_snapshot(snapshot: &GamepadSnapshot) -> Result<(), WireError> {
    if snapshot.pads.len() > MAX_GAMEPADS {
        return Err(WireError::InvalidLength);
    }
    let mut slots = BTreeSet::new();
    for pad in &snapshot.pads {
        if pad.slot >= MAX_GAMEPADS as u8 || !slots.insert(pad.slot) {
            return Err(WireError::InvalidValue(
                "duplicate or out-of-range gamepad slot",
            ));
        }
    }
    Ok(())
}

fn sequence_is_newer(sequence: u32, last: u32) -> bool {
    sequence != last && sequence.wrapping_sub(last) < (u32::MAX / 2)
}

fn is_typematic_key(identity: &KeyIdentity) -> bool {
    !matches!(
        identity.code.as_str(),
        "ShiftLeft"
            | "ShiftRight"
            | "ControlLeft"
            | "ControlRight"
            | "AltLeft"
            | "AltRight"
            | "MetaLeft"
            | "MetaRight"
            | "CapsLock"
            | "NumLock"
            | "ScrollLock"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> KeyIdentity {
        KeyIdentity {
            code: "KeyA".into(),
            key: "a".into(),
        }
    }

    fn roundtrip(message: InputMessage) {
        let encoded = message.encode().unwrap();
        assert_eq!(InputMessage::parse(&encoded), Ok(message));
    }

    #[test]
    fn input_messages_roundtrip() {
        roundtrip(InputMessage::PointerMove {
            sequence: 1,
            point: NormalizedPoint { x: 1, y: 65_535 },
        });
        roundtrip(InputMessage::PointerButton {
            sequence: 2,
            down: true,
            button: PointerButton::Right,
            point: NormalizedPoint { x: 3, y: 4 },
        });
        roundtrip(InputMessage::PointerWheel {
            sequence: 3,
            point: NormalizedPoint { x: 3, y: 4 },
            delta_x: 0.5,
            delta_y: -2.25,
        });
        roundtrip(InputMessage::Key {
            sequence: 4,
            down: true,
            browser_repeat: false,
            identity: key(),
        });
        roundtrip(InputMessage::Text {
            sequence: 5,
            text: "hello å".into(),
        });
        roundtrip(InputMessage::ReleaseAll { sequence: 6 });
        roundtrip(InputMessage::Heartbeat { sequence: 7 });
        roundtrip(InputMessage::GamepadSnapshot {
            sequence: 8,
            snapshot: GamepadSnapshot {
                pads: vec![GamepadState {
                    slot: 0,
                    kind: ControllerKind::DualSense,
                    buttons: 4096,
                    left_trigger: 1,
                    right_trigger: 2,
                    left_x: -3,
                    left_y: 4,
                    right_x: 5,
                    right_y: -6,
                }],
            },
        });
    }

    #[test]
    fn parser_rejects_malformed_messages_without_panicking() {
        assert_eq!(InputMessage::parse(&[]), Err(WireError::Empty));
        assert_eq!(
            InputMessage::parse(&[3, 1, 0, 0, 0, 0]),
            Err(WireError::UnsupportedVersion(3))
        );
        assert_eq!(
            InputMessage::parse(&[4, 0x40, 0, 0, 0, 0]),
            Err(WireError::UnknownKind(0x40))
        );
        assert!(matches!(
            InputMessage::parse(&[4, 1, 0]),
            Err(WireError::Truncated)
        ));
        assert_eq!(
            InputMessage::parse(&[4, 6, 0, 0, 0, 0, 0]),
            Err(WireError::InvalidLength)
        );

        let mut bad_flags = InputMessage::Key {
            sequence: 1,
            down: true,
            browser_repeat: false,
            identity: key(),
        }
        .encode()
        .unwrap();
        bad_flags[6] = 0x80;
        assert_eq!(
            InputMessage::parse(&bad_flags),
            Err(WireError::InvalidFlags)
        );

        let mut bad_pad = InputMessage::GamepadSnapshot {
            sequence: 1,
            snapshot: GamepadSnapshot {
                pads: vec![
                    GamepadState {
                        slot: 0,
                        kind: ControllerKind::Xbox360,
                        buttons: 0,
                        left_trigger: 0,
                        right_trigger: 0,
                        left_x: 0,
                        left_y: 0,
                        right_x: 0,
                        right_y: 0,
                    },
                    GamepadState {
                        slot: 1,
                        kind: ControllerKind::Xbox360,
                        buttons: 0,
                        left_trigger: 0,
                        right_trigger: 0,
                        left_x: 0,
                        left_y: 0,
                        right_x: 0,
                        right_y: 0,
                    },
                ],
            },
        }
        .encode()
        .unwrap();
        bad_pad[7 + GAMEPAD_ENTRY_BYTES] = 0;
        assert!(matches!(
            InputMessage::parse(&bad_pad),
            Err(WireError::InvalidValue(
                "duplicate or out-of-range gamepad slot"
            ))
        ));
    }

    #[test]
    fn encode_rejects_unencodable_messages() {
        assert!(matches!(
            InputMessage::Text {
                sequence: 1,
                text: String::new(),
            }
            .encode(),
            Err(WireError::InvalidLength)
        ));
        assert!(matches!(
            InputMessage::PointerWheel {
                sequence: 1,
                point: NormalizedPoint { x: 0, y: 0 },
                delta_x: f32::NAN,
                delta_y: 0.0,
            }
            .encode(),
            Err(WireError::InvalidValue("non-finite wheel delta"))
        ));
    }

    #[test]
    fn parser_never_panics_on_truncation_or_byte_flips() {
        let encoded = InputMessage::GamepadSnapshot {
            sequence: 8,
            snapshot: GamepadSnapshot {
                pads: vec![GamepadState {
                    slot: 0,
                    kind: ControllerKind::DualSense,
                    buttons: 4096,
                    left_trigger: 1,
                    right_trigger: 2,
                    left_x: -3,
                    left_y: 4,
                    right_x: 5,
                    right_y: -6,
                }],
            },
        }
        .encode()
        .unwrap();
        for len in 0..encoded.len() {
            let _ = InputMessage::parse(&encoded[..len]);
        }
        for index in 0..encoded.len() {
            let mut mutated = encoded.clone();
            mutated[index] ^= 0x55;
            let _ = InputMessage::parse(&mutated);
        }
    }

    #[test]
    fn key_repeats_are_host_generated_without_catchup_bursts() {
        let mut machine = InputStateMachine::new();
        let identity = key();
        let down = InputMessage::Key {
            sequence: 1,
            down: true,
            browser_repeat: false,
            identity: identity.clone(),
        };
        assert_eq!(
            machine.apply(&down, 0),
            vec![HostAction::KeyDown(identity.clone())]
        );
        assert!(machine.tick(499).is_empty());
        assert_eq!(
            machine.tick(500),
            vec![HostAction::KeyRepeat(identity.clone())]
        );
        assert!(machine.tick(532).is_empty());
        assert_eq!(
            machine.tick(1_000),
            vec![HostAction::KeyRepeat(identity.clone())]
        );
    }

    #[test]
    fn modifiers_do_not_typematic_repeat() {
        let mut machine = InputStateMachine::new();
        let identity = KeyIdentity {
            code: "ShiftLeft".into(),
            key: "Shift".into(),
        };
        machine.apply(
            &InputMessage::Key {
                sequence: 1,
                down: true,
                browser_repeat: false,
                identity,
            },
            0,
        );
        assert!(machine.tick(600).is_empty());
    }

    #[test]
    fn release_all_is_idempotent_and_releases_keys_buttons_and_pads() {
        let mut machine = InputStateMachine::new();
        let identity = key();
        machine.apply(
            &InputMessage::Key {
                sequence: 1,
                down: true,
                browser_repeat: false,
                identity: identity.clone(),
            },
            0,
        );
        machine.apply(
            &InputMessage::PointerButton {
                sequence: 2,
                down: true,
                button: PointerButton::Left,
                point: NormalizedPoint { x: 0, y: 0 },
            },
            1,
        );
        machine.apply(
            &InputMessage::GamepadSnapshot {
                sequence: 3,
                snapshot: GamepadSnapshot {
                    pads: vec![GamepadState {
                        slot: 0,
                        kind: ControllerKind::Xbox360,
                        buttons: 1,
                        left_trigger: 0,
                        right_trigger: 0,
                        left_x: 0,
                        left_y: 0,
                        right_x: 0,
                        right_y: 0,
                    }],
                },
            },
            2,
        );

        let actions = machine.apply(&InputMessage::ReleaseAll { sequence: 4 }, 3);
        assert!(actions.contains(&HostAction::KeyUp(identity)));
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::PointerButtonUp {
                button: PointerButton::Left,
                ..
            }
        )));
        assert!(actions.contains(&HostAction::NeutralizeGamepads));
        assert!(
            machine
                .apply(&InputMessage::ReleaseAll { sequence: 5 }, 4)
                .is_empty()
        );
    }

    #[test]
    fn liveness_timeout_releases_once() {
        let mut machine = InputStateMachine::new();
        let identity = key();
        machine.apply(
            &InputMessage::Key {
                sequence: 1,
                down: true,
                browser_repeat: false,
                identity: identity.clone(),
            },
            0,
        );
        assert!(
            machine
                .tick(2_000)
                .iter()
                .all(|action| !matches!(action, HostAction::KeyUp(_)))
        );
        let actions = machine.tick(2_001);
        assert_eq!(actions, vec![HostAction::KeyUp(identity)]);
        assert!(machine.tick(3_000).is_empty());
    }

    #[test]
    fn stale_motion_is_dropped() {
        let mut machine = InputStateMachine::new();
        let point = NormalizedPoint { x: 1, y: 1 };
        let newer = InputMessage::PointerMove {
            sequence: 10,
            point,
        };
        let older = InputMessage::PointerMove { sequence: 9, point };
        assert_eq!(
            machine.apply(&newer, 0),
            vec![HostAction::PointerMove(point)]
        );
        assert!(machine.apply(&older, 1).is_empty());
    }
}
