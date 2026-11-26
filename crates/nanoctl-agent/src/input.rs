use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
#[cfg(not(target_os = "windows"))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::Deserialize;
use xcap::Monitor;

#[cfg(target_os = "linux")]
#[path = "x11_text.rs"]
mod x11_text;

#[cfg(target_os = "windows")]
mod win_send {
    //! Raw `SendInput` for absolute moves and wheel deltas. enigo 0.6 maps
    //! absolute coordinates onto the primary display only (no
    //! `MOUSEEVENTF_VIRTUALDESK`), so input intended for a secondary monitor
    //! lands on the wrong screen, and it can only emit whole wheel notches,
    //! which discards fine-grained precision-trackpad deltas.

    use anyhow::Result;

    const INPUT_MOUSE: u32 = 0;
    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    const KEYEVENTF_SCANCODE: u32 = 0x0008;
    const MOUSEEVENTF_MOVE: u32 = 0x0001;
    const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
    const MOUSEEVENTF_WHEEL: u32 = 0x0800;
    const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
    const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
    const SM_XVIRTUALSCREEN: i32 = 76;
    const SM_YVIRTUALSCREEN: i32 = 77;
    const SM_CXVIRTUALSCREEN: i32 = 78;
    const SM_CYVIRTUALSCREEN: i32 = 79;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseInput {
        dx: i32,
        dy: i32,
        mouse_data: u32,
        flags: u32,
        time: u32,
        extra_info: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Input {
        kind: u32,
        padding: u32,
        mouse: MouseInput,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeybdInput {
        vk: u16,
        scan: u16,
        flags: u32,
        time: u32,
        extra_info: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyboardInput {
        kind: u32,
        padding: u32,
        keyboard: KeybdInput,
        tail: usize,
    }

    const _: () = assert!(std::mem::size_of::<KeyboardInput>() == std::mem::size_of::<Input>());

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendInput(count: u32, inputs: *const std::ffi::c_void, size: i32) -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    fn send<T>(input: &T) -> Result<()> {
        let sent = unsafe {
            SendInput(
                1,
                input as *const T as *const std::ffi::c_void,
                std::mem::size_of::<T>() as i32,
            )
        };
        if sent != 1 {
            return Err(enigo::InputError::Simulate("SendInput rejected the input event").into());
        }
        Ok(())
    }

    fn event(flags: u32, data: i32, dx: i32, dy: i32) -> Input {
        Input {
            kind: INPUT_MOUSE,
            padding: 0,
            mouse: MouseInput {
                dx,
                dy,
                mouse_data: data as u32,
                flags,
                time: 0,
                extra_info: 0,
            },
        }
    }

    /// Absolute move across the whole virtual desktop (all monitors).
    pub fn move_abs(x: i32, y: i32) -> Result<()> {
        let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        anyhow::ensure!(vw > 1 && vh > 1, "virtual screen metrics unavailable");
        let nx = virtual_normalized(x - vx, vw);
        let ny = virtual_normalized(y - vy, vh);
        send(&event(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            0,
            nx,
            ny,
        ))
    }

    /// Relative mouse motion in desktop pixels, used by pointer-lock/game mouse mode.
    pub fn move_rel(dx: i32, dy: i32) -> Result<()> {
        send(&event(MOUSEEVENTF_MOVE, 0, dx, dy))
    }

    /// One wheel event in raw wheel-delta units (120 = one notch).
    pub fn wheel(units: i32, horizontal: bool) -> Result<()> {
        let (flags, data) = if horizontal {
            (MOUSEEVENTF_HWHEEL, units)
        } else {
            // Positive mouseData scrolls up; viewer deltas are down-positive.
            (MOUSEEVENTF_WHEEL, -units)
        };
        send(&event(flags, data, 0, 0))
    }

    /// Scan-set-1 extended-key marker used internally before SendInput.
    pub const EXTENDED_SCAN: u16 = 0xe000;

    /// Raw scan-code key input. `scan` uses `EXTENDED_SCAN` as a marker for
    /// E0-prefixed keys; SendInput receives only the low byte plus the flag.
    pub fn key(scan: u16, press: bool) -> Result<()> {
        let extended = scan & EXTENDED_SCAN != 0;
        let scan = scan & 0xff;
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !press {
            flags |= KEYEVENTF_KEYUP;
        }
        send(&KeyboardInput {
            kind: INPUT_KEYBOARD,
            padding: 0,
            keyboard: KeybdInput {
                vk: 0,
                scan,
                flags,
                time: 0,
                extra_info: 0,
            },
            tail: 0,
        })
    }

    fn virtual_normalized(offset: i32, extent: i32) -> i32 {
        let extent = i64::from(extent) - 1;
        ((i64::from(offset) * 65535 + extent / 2) / extent).clamp(0, 65535) as i32
    }
}

#[derive(Clone, Copy)]
pub enum InputLane {
    Reliable,
    PointerMotion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum InjectedKey {
    Mapped(Key),
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    ScanCode(u16),
}

pub struct InputController {
    enigo: Enigo,
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    held_keys: HashSet<InjectedKey>,
    repeat_keys: HashMap<InjectedKey, Instant>,
    held_buttons: HashSet<Button>,
    last_physical_input: Instant,
    gamepads: crate::gamepad::Gamepads,
    gamepads_revoked: bool,
    #[cfg(not(target_os = "windows"))]
    scroll_remainder_x: f64,
    #[cfg(not(target_os = "windows"))]
    scroll_remainder_y: f64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ControlMessage {
    Pointer {
        action: PointerAction,
        x: f64,
        y: f64,
        button: Option<u8>,
        #[serde(rename = "deltaX")]
        delta_x: Option<f64>,
        #[serde(rename = "deltaY")]
        delta_y: Option<f64>,
    },
    Key {
        action: KeyAction,
        code: String,
        key: String,
        repeat: bool,
    },
    Release,
    Text {
        text: String,
    },
    Ping,
    Gamepad(crate::gamepad::Snapshot),
    GamepadHeartbeat,
    Display {
        #[serde(rename = "displayId")]
        display_id: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PointerAction {
    Move,
    Down,
    Up,
    Wheel,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum KeyAction {
    Down,
    Up,
}

/// The host synthesizes typematic repeats while a key is physically held.
/// Sending every browser repeat over SCTP costs a full datagram for no extra
/// information. These match the common desktop default closely enough for
/// remote input; the exact client OS setting is intentionally not replicated.
const KEY_REPEAT_DELAY: Duration = Duration::from_millis(500);
const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(33);

const BINARY_POINTER_MOVE: u8 = 1;
const BINARY_GAMEPAD: u8 = 2;
const BINARY_POINTER_BUTTON: u8 = 3;
const BINARY_POINTER_WHEEL: u8 = 4;
const BINARY_KEY: u8 = 5;
const BINARY_TEXT: u8 = 6;
const BINARY_RELEASE: u8 = 7;
const BINARY_PING: u8 = 8;
const BINARY_AUDIO: u8 = 9;
const BINARY_DISPLAY: u8 = 10;
const BINARY_GAMEPAD_HEARTBEAT: u8 = 11;
const BINARY_GAMEPAD_SPARSE: u8 = 12;
const BINARY_KEY_EX: u8 = 13;
const BINARY_POINTER_RELATIVE: u8 = 14;
const BINARY_KEY_NAMED: u8 = 4;

pub(crate) fn is_binary_input_message(bytes: &[u8]) -> bool {
    bytes
        .first()
        .is_some_and(|kind| (BINARY_POINTER_MOVE..=BINARY_POINTER_RELATIVE).contains(kind))
}

fn binary_named_key(id: u8) -> Option<&'static str> {
    match id {
        1 => Some("Backspace"),
        2 => Some("Delete"),
        3 => Some("Enter"),
        4 => Some("Escape"),
        5 => Some("Tab"),
        6 => Some("Space"),
        7 => Some("ArrowUp"),
        8 => Some("ArrowDown"),
        9 => Some("ArrowLeft"),
        10 => Some("ArrowRight"),
        11 => Some("Home"),
        12 => Some("End"),
        13 => Some("PageUp"),
        14 => Some("PageDown"),
        15..=26 => Some(match id {
            15 => "F1",
            16 => "F2",
            17 => "F3",
            18 => "F4",
            19 => "F5",
            20 => "F6",
            21 => "F7",
            22 => "F8",
            23 => "F9",
            24 => "F10",
            25 => "F11",
            _ => "F12",
        }),
        27 => Some("CapsLock"),
        28 => Some("PrintScreen"),
        29 => Some("Insert"),
        30 => Some("ShiftLeft"),
        31 => Some("ControlLeft"),
        32 => Some("AltLeft"),
        33 => Some("MetaLeft"),
        _ => None,
    }
}

/// Version-3 compact input wire format. Datagram channels already preserve
/// message boundaries, so byte zero is the kind and there is no magic/version
/// framing. All integer and floating-point fields are little-endian.
fn parse_binary_control(bytes: &[u8]) -> Result<ControlMessage> {
    if bytes.is_empty() {
        anyhow::bail!("empty binary input message");
    }
    let u16_at = |offset: usize| -> Result<u16> {
        let bytes = bytes
            .get(offset..offset + 2)
            .context("truncated binary input u16")?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let compact_f64_at = |offset: usize, len: usize| -> Result<f64> {
        let bytes = bytes
            .get(offset..offset + len)
            .context("truncated compact binary input f64")?;
        let mut wide = [0_u8; 8];
        wide[..len].copy_from_slice(bytes);
        Ok(f64::from_be_bytes(wide))
    };
    let position = |x: u16, y: u16| (f64::from(x) / 65535.0, f64::from(y) / 65535.0);
    match bytes[0] {
        BINARY_POINTER_MOVE => {
            if bytes.len() != 5 {
                anyhow::bail!("invalid binary pointer move length");
            }
            let (x, y) = position(u16_at(1)?, u16_at(3)?);
            Ok(ControlMessage::Pointer {
                action: PointerAction::Move,
                x,
                y,
                button: None,
                delta_x: None,
                delta_y: None,
            })
        }
        BINARY_POINTER_RELATIVE => {
            if bytes.len() != 5 {
                anyhow::bail!("invalid binary relative pointer length");
            }
            let dx = i16::from_le_bytes([bytes[1], bytes[2]]);
            let dy = i16::from_le_bytes([bytes[3], bytes[4]]);
            if dx == 0 && dy == 0 {
                anyhow::bail!("empty binary relative pointer motion");
            }
            Ok(ControlMessage::Pointer {
                action: PointerAction::Move,
                x: 0.0,
                y: 0.0,
                button: None,
                delta_x: Some(f64::from(dx)),
                delta_y: Some(f64::from(dy)),
            })
        }
        BINARY_GAMEPAD | BINARY_GAMEPAD_SPARSE => {
            crate::gamepad::Snapshot::parse_binary(&bytes[1..], bytes[0] == BINARY_GAMEPAD_SPARSE)
                .map(ControlMessage::Gamepad)
                .map_err(anyhow::Error::msg)
        }
        BINARY_POINTER_BUTTON => {
            if bytes.len() != 6 {
                anyhow::bail!("invalid binary pointer button length");
            }
            let control = bytes[1];
            let button = control & 0x03;
            if button > 2 || control & 0x3c != 0 {
                anyhow::bail!("invalid binary pointer button flags");
            }
            let (x, y) = position(u16_at(2)?, u16_at(4)?);
            Ok(ControlMessage::Pointer {
                action: if control & 0x80 != 0 {
                    PointerAction::Up
                } else {
                    PointerAction::Down
                },
                x,
                y,
                button: Some(button),
                delta_x: (control & 0x40 != 0).then_some(0.0),
                delta_y: (control & 0x40 != 0).then_some(0.0),
            })
        }
        BINARY_POINTER_WHEEL => {
            if bytes.len() < 6 {
                anyhow::bail!("truncated binary wheel input");
            }
            let x_len = usize::from(bytes[5] & 0x0f);
            let y_len = usize::from(bytes[5] >> 4);
            if x_len > 8 || y_len > 8 || bytes.len() != 6 + x_len + y_len {
                anyhow::bail!("invalid binary wheel length");
            }
            let (x, y) = position(u16_at(1)?, u16_at(3)?);
            Ok(ControlMessage::Pointer {
                action: PointerAction::Wheel,
                x,
                y,
                button: None,
                delta_x: Some(compact_f64_at(6, x_len)?),
                delta_y: Some(compact_f64_at(6 + x_len, y_len)?),
            })
        }
        BINARY_KEY => {
            if bytes.len() < 2 {
                anyhow::bail!("truncated binary key input");
            }
            let flags = bytes[1];
            if flags & !7 != 0 {
                anyhow::bail!("invalid binary key flags");
            }
            let (code, key) = if flags & BINARY_KEY_NAMED != 0 {
                if bytes.len() != 3 {
                    anyhow::bail!("invalid binary named key length");
                }
                let code = binary_named_key(bytes[2]).context("unknown binary named key")?;
                (code.to_owned(), code.to_owned())
            } else {
                if bytes.len() <= 2 || bytes.len() > 34 {
                    anyhow::bail!("invalid binary key length");
                }
                let key = std::str::from_utf8(&bytes[2..])
                    .context("binary key value is not UTF-8")?
                    .to_owned();
                (String::new(), key)
            };
            Ok(ControlMessage::Key {
                action: if flags & 1 != 0 {
                    KeyAction::Up
                } else {
                    KeyAction::Down
                },
                code,
                key,
                repeat: flags & 2 != 0,
            })
        }
        BINARY_KEY_EX => {
            if bytes.len() < 4 {
                anyhow::bail!("truncated extended binary key input");
            }
            let flags = bytes[1];
            if flags & !3 != 0 {
                anyhow::bail!("invalid extended binary key flags");
            }
            let code_len = usize::from(bytes[2]);
            let key_len = usize::from(bytes[3]);
            if bytes.len() != 4 + code_len + key_len || key_len == 0 {
                anyhow::bail!("invalid extended binary key length");
            }
            let code = std::str::from_utf8(&bytes[4..4 + code_len])
                .context("extended binary key code is not UTF-8")?
                .to_owned();
            let key = std::str::from_utf8(&bytes[4 + code_len..])
                .context("extended binary key value is not UTF-8")?
                .to_owned();
            Ok(ControlMessage::Key {
                action: if flags & 1 != 0 {
                    KeyAction::Up
                } else {
                    KeyAction::Down
                },
                code,
                key,
                repeat: flags & 2 != 0,
            })
        }
        BINARY_TEXT => {
            if bytes.len() < 3 {
                anyhow::bail!("truncated binary text input");
            }
            let len = usize::from(u16_at(1)?);
            if bytes.len() != 3 + len {
                anyhow::bail!("invalid binary text length");
            }
            Ok(ControlMessage::Text {
                text: std::str::from_utf8(&bytes[3..])
                    .context("binary text is not UTF-8")?
                    .to_owned(),
            })
        }
        BINARY_RELEASE => {
            if bytes.len() != 1 {
                anyhow::bail!("invalid binary release length");
            }
            Ok(ControlMessage::Release)
        }
        BINARY_GAMEPAD_HEARTBEAT => {
            if bytes.len() != 1 {
                anyhow::bail!("invalid binary gamepad heartbeat length");
            }
            Ok(ControlMessage::GamepadHeartbeat)
        }
        BINARY_PING => {
            if bytes.len() != 1 {
                anyhow::bail!("invalid binary ping length");
            }
            Ok(ControlMessage::Ping)
        }
        BINARY_AUDIO => {
            if bytes.len() != 2 {
                anyhow::bail!("invalid binary audio length");
            }
            Ok(ControlMessage::Unsupported)
        }
        BINARY_DISPLAY => {
            if bytes.len() < 2 {
                anyhow::bail!("truncated binary display input");
            }
            let len = usize::from(bytes[1]);
            if bytes.len() != 2 + len {
                anyhow::bail!("invalid binary display length");
            }
            Ok(ControlMessage::Display {
                display_id: std::str::from_utf8(&bytes[2..])
                    .context("binary display identifier is not UTF-8")?
                    .to_owned(),
            })
        }
        _ => anyhow::bail!("unknown binary input message type"),
    }
}

impl InputController {
    #[cfg(target_os = "windows")]
    pub(crate) fn display_geometry(&self) -> (i32, i32, u32, u32) {
        (self.origin_x, self.origin_y, self.width, self.height)
    }

    pub fn new() -> Result<Self> {
        let monitors = Monitor::all().context("cannot enumerate displays for input")?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .context("no display available for input")?;
        Ok(Self {
            enigo: Enigo::new(&Settings::default()).context("input injection is unavailable")?,
            origin_x: monitor.x()?,
            origin_y: monitor.y()?,
            width: monitor.width()?,
            height: monitor.height()?,
            held_keys: HashSet::new(),
            repeat_keys: HashMap::new(),
            held_buttons: HashSet::new(),
            last_physical_input: Instant::now(),
            gamepads: Default::default(),
            gamepads_revoked: false,
            #[cfg(not(target_os = "windows"))]
            scroll_remainder_x: 0.0,
            #[cfg(not(target_os = "windows"))]
            scroll_remainder_y: 0.0,
        })
    }

    pub fn dispatch(&mut self, bytes: &[u8], lane: InputLane) -> Result<Option<String>> {
        if bytes.len() > crate::rtc::MAX_CONTROL_MESSAGE_BYTES {
            anyhow::bail!("control message exceeds maximum size");
        }
        let message: ControlMessage = if is_binary_input_message(bytes) {
            parse_binary_control(bytes)?
        } else {
            anyhow::bail!("control message is not input wire v3");
        };
        match (&message, lane) {
            (
                ControlMessage::Pointer {
                    action: PointerAction::Move,
                    ..
                },
                InputLane::PointerMotion,
            )
            | (
                ControlMessage::Gamepad(_) | ControlMessage::GamepadHeartbeat,
                InputLane::PointerMotion,
            ) => {}
            (
                ControlMessage::Pointer {
                    action: PointerAction::Move,
                    ..
                },
                InputLane::Reliable,
            ) => {
                anyhow::bail!("pointer motion must use the pointer channel");
            }
            (_, InputLane::PointerMotion) => {
                anyhow::bail!(
                    "only pointer motion and gamepad reports are accepted on the pointer channel"
                );
            }
            (_, InputLane::Reliable) => {}
        }
        if !self.gamepads_revoked {
            // Any well-formed viewer input packet proves the reverse path is
            // alive exactly like a gamepad heartbeat. Sharing that liveness
            // lets clients skip redundant heartbeat datagrams while keys,
            // text, pointer buttons, or control traffic are already flowing.
            self.gamepads.heartbeat();
        }
        if !matches!(
            &message,
            ControlMessage::Ping
                | ControlMessage::Release
                | ControlMessage::Gamepad(_)
                | ControlMessage::GamepadHeartbeat
                | ControlMessage::Unsupported
        ) {
            self.last_physical_input = Instant::now();
        }
        match message {
            ControlMessage::Pointer {
                action,
                x,
                y,
                button,
                delta_x,
                delta_y,
            } => {
                self.pointer(action, x, y, button, delta_x, delta_y)?;
                Ok(None)
            }
            ControlMessage::Key {
                action,
                code,
                key,
                repeat,
            } => {
                self.keyboard(action, &code, &key, repeat)?;
                Ok(None)
            }
            ControlMessage::Release => {
                self.release_all();
                Ok(None)
            }
            ControlMessage::Text { text } => {
                #[cfg(target_os = "linux")]
                if std::env::var_os("DISPLAY").is_some()
                    && std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland")
                {
                    x11_text::inject(&text)?;
                    return Ok(None);
                }
                self.enigo.text(&text)?;
                Ok(None)
            }
            ControlMessage::Ping => Ok(None),
            ControlMessage::Gamepad(snapshot) => {
                if !self.gamepads_revoked {
                    self.gamepads.update(snapshot)?;
                }
                Ok(None)
            }
            ControlMessage::GamepadHeartbeat => {
                if !self.gamepads_revoked {
                    self.gamepads.heartbeat();
                }
                Ok(None)
            }
            ControlMessage::Display { display_id } => {
                self.select_display(&display_id)?;
                Ok(Some(display_id))
            }
            ControlMessage::Unsupported => Ok(None),
        }
    }

    pub(crate) fn select_display(&mut self, display_id: &str) -> Result<()> {
        if display_id.len() > 128 {
            anyhow::bail!("display identifier is too long");
        }
        let monitor = Monitor::all()
            .context("cannot enumerate displays for input")?
            .into_iter()
            .find(|monitor| monitor.id().is_ok_and(|id| id.to_string() == display_id))
            .context("selected display is unavailable")?;
        self.release_all();
        self.origin_x = monitor.x()?;
        self.origin_y = monitor.y()?;
        self.width = monitor.width()?;
        self.height = monitor.height()?;
        Ok(())
    }

    fn pointer(
        &mut self,
        action: PointerAction,
        x: f64,
        y: f64,
        button: Option<u8>,
        delta_x: Option<f64>,
        delta_y: Option<f64>,
    ) -> Result<()> {
        let x = display_pixel(x, self.width, self.origin_x);
        let y = display_pixel(y, self.height, self.origin_y);
        match action {
            PointerAction::Move => {
                if delta_x.is_some() || delta_y.is_some() {
                    self.move_by(delta_x.unwrap_or(0.0), delta_y.unwrap_or(0.0))?;
                } else {
                    self.move_to(x, y)?;
                }
            }
            PointerAction::Down => {
                if delta_x.is_none() && delta_y.is_none() {
                    self.move_to(x, y)?;
                }
                let button = map_button(button)?;
                self.enigo.button(button, Direction::Press)?;
                self.held_buttons.insert(button);
            }
            PointerAction::Up => {
                if delta_x.is_none() && delta_y.is_none() {
                    self.move_to(x, y)?;
                }
                let button = map_button(button)?;
                self.held_buttons.remove(&button);
                self.enigo.button(button, Direction::Release)?;
            }
            PointerAction::Wheel => {
                // Wheel input reaches the window under the physical cursor,
                // so it has to arrive where the viewer's pointer is first.
                self.move_to(x, y)?;
                self.wheel(delta_x.unwrap_or(0.0), Axis::Horizontal)?;
                self.wheel(delta_y.unwrap_or(0.0), Axis::Vertical)?;
            }
        }
        Ok(())
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            win_send::move_abs(x, y)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(self.enigo.move_mouse(x, y, Coordinate::Abs)?)
        }
    }

    fn move_by(&mut self, dx: f64, dy: f64) -> Result<()> {
        let dx = dx.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i32;
        let dy = dy.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i32;
        #[cfg(target_os = "windows")]
        {
            win_send::move_rel(dx, dy)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(self.enigo.move_mouse(dx, dy, Coordinate::Rel)?)
        }
    }

    /// Wheel deltas arrive in pixels. On Windows they are sent as raw wheel
    /// units so precision-trackpad deltas accumulate like real hardware
    /// input; elsewhere whole notches are emitted once enough pixels accrue.
    fn wheel(&mut self, pixels: f64, axis: Axis) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            let units = wheel_units(pixels);
            if units != 0 {
                win_send::wheel(units, matches!(axis, Axis::Horizontal))?;
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let remainder = if matches!(axis, Axis::Horizontal) {
                &mut self.scroll_remainder_x
            } else {
                &mut self.scroll_remainder_y
            };
            let notches = accrued_notches(remainder, pixels);
            if notches != 0 {
                self.enigo.scroll(notches, axis)?;
            }
        }
        Ok(())
    }

    fn keyboard(&mut self, action: KeyAction, code: &str, value: &str, repeat: bool) -> Result<()> {
        if code.len() > 64 || value.len() > 32 {
            anyhow::bail!("key identifier is too long");
        }
        let Some(key) = injected_key(code, value) else {
            return Ok(());
        };
        match action {
            // Typematic repeats are generated locally while the key remains
            // held. A repeat datagram is only a clock tick and is ignored.
            KeyAction::Down if repeat => {}
            KeyAction::Down if !self.held_keys.contains(&key) => {
                inject_key(&mut self.enigo, key, Direction::Press)?;
                self.held_keys.insert(key);
                if repeatable_injected_key(key) {
                    self.repeat_keys
                        .insert(key, Instant::now() + KEY_REPEAT_DELAY);
                }
            }
            KeyAction::Up => {
                self.held_keys.remove(&key);
                self.repeat_keys.remove(&key);
                inject_key(&mut self.enigo, key, Direction::Release)?;
            }
            KeyAction::Down => {}
        }
        Ok(())
    }

    /// Emits host-side typematic repeats whose deadline has passed. The RTC
    /// watchdog calls this frequently; it is a no-op unless a repeatable key
    /// is still held.
    pub(crate) fn pump_key_repeats(&mut self, now: Instant) {
        for (key, due) in &mut self.repeat_keys {
            if key_repeat_due(due, now) {
                let _ = inject_key(&mut self.enigo, *key, Direction::Press);
            }
        }
    }

    pub fn release_all(&mut self) {
        self.repeat_keys.clear();
        for key in self.held_keys.drain() {
            let _ = inject_key(&mut self.enigo, key, Direction::Release);
        }
        for button in self.held_buttons.drain() {
            let _ = self.enigo.button(button, Direction::Release);
        }
    }

    pub fn release_if_idle(&mut self, timeout: Duration, gamepad_timeout: Duration) {
        self.gamepads.neutralize_if_idle(gamepad_timeout);
        if self.last_physical_input.elapsed() >= timeout
            && (!self.held_keys.is_empty() || !self.held_buttons.is_empty())
        {
            self.release_all();
        }
    }

    /// Idle every emulated pad without unplugging it: the right response to a
    /// transient control-channel loss, as opposed to `revoke_gamepads` which
    /// detaches the virtual controllers for good.
    pub fn neutralize_gamepads(&mut self) {
        self.gamepads.neutralize();
    }

    pub fn revoke_gamepads(&mut self) {
        self.gamepads_revoked = true;
        self.gamepads = Default::default();
    }
}

pub fn probe() -> Result<()> {
    InputController::new().map(drop)
}

impl Drop for InputController {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn normalized_pixel(value: f64, extent: u32) -> i32 {
    let normalized = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (normalized * f64::from(extent.saturating_sub(1))).round() as i32
}

fn display_pixel(value: f64, extent: u32, origin: i32) -> i32 {
    origin.saturating_add(normalized_pixel(value, extent))
}

/// Pixels per wheel notch: browsers report a physical wheel notch as
/// roughly 100 CSS pixels, and one notch is 120 wheel-delta units.
const PIXELS_PER_NOTCH: f64 = 100.0;

/// Converts a pixel scroll delta into raw wheel-delta units, bounded to
/// twenty notches per event. Fractional units are kept so fine-grained
/// trackpad deltas scroll smoothly instead of rounding to zero.
#[cfg(any(target_os = "windows", test))]
fn wheel_units(pixels: f64) -> i32 {
    if !pixels.is_finite() {
        return 0;
    }
    (pixels * 120.0 / PIXELS_PER_NOTCH)
        .round()
        .clamp(-2_400.0, 2_400.0) as i32
}

/// Accumulates pixel scroll deltas and reports how many whole notches are
/// due; the sub-notch remainder is carried into the next event.
#[cfg(any(not(target_os = "windows"), test))]
fn accrued_notches(remainder: &mut f64, pixels: f64) -> i32 {
    if pixels.is_finite() {
        *remainder = (*remainder + pixels).clamp(-4_800.0, 4_800.0);
    }
    let notches = (*remainder / PIXELS_PER_NOTCH).trunc() as i32;
    *remainder -= f64::from(notches) * PIXELS_PER_NOTCH;
    notches
}

fn map_button(value: Option<u8>) -> Result<Button> {
    match value.unwrap_or(0) {
        0 => Ok(Button::Left),
        1 => Ok(Button::Middle),
        2 => Ok(Button::Right),
        _ => anyhow::bail!("unsupported pointer button"),
    }
}

fn key_repeat_due(due: &mut Instant, now: Instant) -> bool {
    if now < *due {
        return false;
    }
    *due = now + KEY_REPEAT_INTERVAL;
    true
}

fn repeatable_key(key: Key) -> bool {
    !matches!(
        key,
        Key::Shift | Key::Control | Key::Alt | Key::Meta | Key::CapsLock
    )
}

#[cfg(target_os = "windows")]
fn scan_key(scan: u16, direction: Direction) -> Result<()> {
    match direction {
        Direction::Press => win_send::key(scan, true)?,
        Direction::Release => win_send::key(scan, false)?,
        Direction::Click => {
            win_send::key(scan, true)?;
            win_send::key(scan, false)?;
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn scan_key(_scan: u16, _direction: Direction) -> Result<()> {
    unreachable!("scan-code keys are only selected on Windows")
}

fn inject_key(enigo: &mut Enigo, key: InjectedKey, direction: Direction) -> Result<()> {
    match key {
        InjectedKey::Mapped(key) => enigo.key(key, direction)?,
        InjectedKey::ScanCode(scan) => scan_key(scan, direction)?,
    }
    Ok(())
}

fn injected_key(code: &str, value: &str) -> Option<InjectedKey> {
    #[cfg(target_os = "windows")]
    if let Some(scan) = windows_scan_code(code) {
        return Some(InjectedKey::ScanCode(scan));
    }
    map_key(code, value).map(InjectedKey::Mapped)
}

fn repeatable_injected_key(key: InjectedKey) -> bool {
    match key {
        InjectedKey::Mapped(key) => repeatable_key(key),
        InjectedKey::ScanCode(scan) => !matches!(
            scan,
            0x1d | 0x2a | 0x36 | 0x38 | 0x3a | 0x45 | 0xe01d | 0xe038 | 0xe05b | 0xe05c
        ),
    }
}

/// Browser `KeyboardEvent.code` positions as Windows scan-set-1 codes. Raw
/// scan input keeps game controls positional (WASD stays WASD) instead of
/// depending on whichever character the host layout assigns. E0-prefixed keys
/// carry the internal `0xe000` marker and are emitted with the extended flag.
#[cfg(any(target_os = "windows", test))]
fn windows_scan_code(code: &str) -> Option<u16> {
    Some(match code {
        "Escape" => 0x01,
        "Digit1" => 0x02,
        "Digit2" => 0x03,
        "Digit3" => 0x04,
        "Digit4" => 0x05,
        "Digit5" => 0x06,
        "Digit6" => 0x07,
        "Digit7" => 0x08,
        "Digit8" => 0x09,
        "Digit9" => 0x0a,
        "Digit0" => 0x0b,
        "Minus" => 0x0c,
        "Equal" => 0x0d,
        "Backspace" => 0x0e,
        "Tab" => 0x0f,
        "KeyQ" => 0x10,
        "KeyW" => 0x11,
        "KeyE" => 0x12,
        "KeyR" => 0x13,
        "KeyT" => 0x14,
        "KeyY" => 0x15,
        "KeyU" => 0x16,
        "KeyI" => 0x17,
        "KeyO" => 0x18,
        "KeyP" => 0x19,
        "BracketLeft" => 0x1a,
        "BracketRight" => 0x1b,
        "Enter" => 0x1c,
        "NumpadEnter" => 0xe01c,
        "ControlLeft" => 0x1d,
        "ControlRight" => 0xe01d,
        "KeyA" => 0x1e,
        "KeyS" => 0x1f,
        "KeyD" => 0x20,
        "KeyF" => 0x21,
        "KeyG" => 0x22,
        "KeyH" => 0x23,
        "KeyJ" => 0x24,
        "KeyK" => 0x25,
        "KeyL" => 0x26,
        "Semicolon" => 0x27,
        "Quote" => 0x28,
        "Backquote" => 0x29,
        "ShiftLeft" => 0x2a,
        "Backslash" => 0x2b,
        "KeyZ" => 0x2c,
        "KeyX" => 0x2d,
        "KeyC" => 0x2e,
        "KeyV" => 0x2f,
        "KeyB" => 0x30,
        "KeyN" => 0x31,
        "KeyM" => 0x32,
        "Comma" => 0x33,
        "Period" => 0x34,
        "Slash" => 0x35,
        "ShiftRight" => 0x36,
        "NumpadMultiply" => 0x37,
        "AltLeft" => 0x38,
        "AltRight" => 0xe038,
        "Space" => 0x39,
        "CapsLock" => 0x3a,
        "F1" => 0x3b,
        "F2" => 0x3c,
        "F3" => 0x3d,
        "F4" => 0x3e,
        "F5" => 0x3f,
        "F6" => 0x40,
        "F7" => 0x41,
        "F8" => 0x42,
        "F9" => 0x43,
        "F10" => 0x44,
        "NumLock" => 0x45,
        "Numpad7" => 0x47,
        "Numpad8" => 0x48,
        "Numpad9" => 0x49,
        "NumpadSubtract" => 0x4a,
        "Numpad4" => 0x4b,
        "Numpad5" => 0x4c,
        "Numpad6" => 0x4d,
        "NumpadAdd" => 0x4e,
        "Numpad1" => 0x4f,
        "Numpad2" => 0x50,
        "Numpad3" => 0x51,
        "Numpad0" => 0x52,
        "NumpadDecimal" => 0x53,
        "NumpadDivide" => 0xe035,
        "IntlBackslash" => 0x56,
        "F11" => 0x57,
        "F12" => 0x58,
        "MetaLeft" => 0xe05b,
        "MetaRight" => 0xe05c,
        "Home" => 0xe047,
        "End" => 0xe04f,
        "PageUp" => 0xe049,
        "PageDown" => 0xe051,
        "Insert" => 0xe052,
        "Delete" => 0xe053,
        "ArrowUp" => 0xe048,
        "ArrowDown" => 0xe050,
        "ArrowLeft" => 0xe04b,
        "ArrowRight" => 0xe04d,
        _ => return None,
    })
}

fn map_key(code: &str, value: &str) -> Option<Key> {
    let named = match code {
        "Backspace" => Some(Key::Backspace),
        "Delete" => Some(Key::Delete),
        "Enter" | "NumpadEnter" => Some(Key::Return),
        "Escape" => Some(Key::Escape),
        "Tab" => Some(Key::Tab),
        "Space" => Some(Key::Space),
        "ArrowUp" => Some(Key::UpArrow),
        "ArrowDown" => Some(Key::DownArrow),
        "ArrowLeft" => Some(Key::LeftArrow),
        "ArrowRight" => Some(Key::RightArrow),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        "F1" => Some(Key::F1),
        "F2" => Some(Key::F2),
        "F3" => Some(Key::F3),
        "F4" => Some(Key::F4),
        "F5" => Some(Key::F5),
        "F6" => Some(Key::F6),
        "F7" => Some(Key::F7),
        "F8" => Some(Key::F8),
        "F9" => Some(Key::F9),
        "F10" => Some(Key::F10),
        "F11" => Some(Key::F11),
        "F12" => Some(Key::F12),
        "CapsLock" => Some(Key::CapsLock),
        #[cfg(not(target_os = "macos"))]
        "PrintScreen" => Some(Key::PrintScr),
        #[cfg(not(target_os = "macos"))]
        "Insert" => Some(Key::Insert),
        "ShiftLeft" | "ShiftRight" => Some(Key::Shift),
        "ControlLeft" | "ControlRight" => Some(Key::Control),
        "AltLeft" | "AltRight" => Some(Key::Alt),
        "MetaLeft" | "MetaRight" => Some(Key::Meta),
        _ => None,
    };
    named.or_else(|| {
        let mut characters = value.chars();
        let character = characters.next()?;
        (characters.next().is_none() && !character.is_control()).then_some(Key::Unicode(character))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_repeat_schedule_and_repeatable_key_classification() {
        let start = Instant::now();
        let mut due = start + KEY_REPEAT_DELAY;
        assert!(!key_repeat_due(
            &mut due,
            start + KEY_REPEAT_DELAY - Duration::from_millis(1)
        ));
        assert!(key_repeat_due(&mut due, start + KEY_REPEAT_DELAY));
        assert_eq!(due, start + KEY_REPEAT_DELAY + KEY_REPEAT_INTERVAL);
        assert!(repeatable_key(Key::Unicode('a')));
        assert!(repeatable_key(Key::Backspace));
        assert!(!repeatable_key(Key::Shift));
        assert!(!repeatable_key(Key::Control));
    }
    use crate::rtc::MAX_CONTROL_MESSAGE_BYTES;
    use proptest::prelude::*;

    #[test]
    fn decodes_binary_relative_pointer_motion_and_buttons() {
        let motion =
            parse_binary_control(&[BINARY_POINTER_RELATIVE, 0xf4, 0xff, 0x2c, 0x01]).unwrap();
        assert!(matches!(
            motion,
            ControlMessage::Pointer {
                action: PointerAction::Move,
                delta_x: Some(-12.0),
                delta_y: Some(300.0),
                ..
            }
        ));
        let button = parse_binary_control(&[BINARY_POINTER_BUTTON, 0x40, 0, 0, 0, 0]).unwrap();
        assert!(matches!(
            button,
            ControlMessage::Pointer {
                action: PointerAction::Down,
                delta_x: Some(0.0),
                delta_y: Some(0.0),
                ..
            }
        ));
        assert!(parse_binary_control(&[BINARY_POINTER_RELATIVE, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn decodes_binary_pointer_motion() {
        let message = parse_binary_control(&[1, 0x00, 0x80, 0xff, 0xff]).unwrap();
        assert!(matches!(
            message,
            ControlMessage::Pointer {
                action: PointerAction::Move,
                x,
                y,
                ..
            } if (x - 32768.0 / 65535.0).abs() < f64::EPSILON && y == 1.0
        ));
        assert!(parse_binary_control(&[1, 0x00, 0x80, 0xff]).is_err());
    }

    #[test]
    fn decodes_binary_gamepad_heartbeat_without_state() {
        assert!(matches!(
            parse_binary_control(&[BINARY_GAMEPAD_HEARTBEAT]).unwrap(),
            ControlMessage::GamepadHeartbeat
        ));
        assert!(parse_binary_control(&[BINARY_GAMEPAD_HEARTBEAT, 0]).is_err());
    }

    #[test]
    fn decodes_utf8_and_named_binary_keys() {
        let utf8 = parse_binary_control(&[BINARY_KEY, 0, b'a']).unwrap();
        assert!(matches!(
            utf8,
            ControlMessage::Key { code, key, .. } if code.is_empty() && key == "a"
        ));
        let named = parse_binary_control(&[BINARY_KEY, 1 | BINARY_KEY_NAMED, 30]).unwrap();
        assert!(matches!(
            named,
            ControlMessage::Key {
                action: KeyAction::Up,
                code,
                ..
            } if code == "ShiftLeft"
        ));
        assert!(parse_binary_control(&[BINARY_KEY, BINARY_KEY_NAMED, 99]).is_err());
    }

    #[test]
    fn decodes_extended_binary_keys_with_physical_codes() {
        let bytes = [BINARY_KEY_EX, 2, 4, 1, b'K', b'e', b'y', b'A', b'a'];
        let message = parse_binary_control(&bytes).unwrap();
        assert!(matches!(
            message,
            ControlMessage::Key {
                action: KeyAction::Down,
                code,
                key,
                repeat: true,
            } if code == "KeyA" && key == "a"
        ));
        assert!(parse_binary_control(&[BINARY_KEY_EX, 4, 4, 1]).is_err());
        assert!(parse_binary_control(&[BINARY_KEY_EX, 0, 4, 0, b'K']).is_err());
    }

    #[test]
    fn maps_browser_codes_to_nonextended_windows_scan_codes() {
        assert_eq!(windows_scan_code("KeyW"), Some(0x11));
        assert_eq!(windows_scan_code("Digit1"), Some(0x02));
        assert_eq!(windows_scan_code("Space"), Some(0x39));
        assert_eq!(windows_scan_code("IntlBackslash"), Some(0x56));
        assert_eq!(windows_scan_code("ArrowLeft"), Some(0xe04b));
        assert_eq!(windows_scan_code("NumpadEnter"), Some(0xe01c));
        assert_eq!(windows_scan_code("ControlRight"), Some(0xe01d));
        assert_eq!(windows_scan_code("PrintScreen"), None);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_injects_positional_keys_as_scan_codes() {
        assert!(matches!(
            injected_key("KeyW", "w"),
            Some(InjectedKey::ScanCode(0x11))
        ));
        assert!(matches!(
            injected_key("ArrowLeft", "ArrowLeft"),
            Some(InjectedKey::ScanCode(0xe04b))
        ));
        assert!(matches!(
            injected_key("UnknownKey", "é"),
            Some(InjectedKey::Mapped(Key::Unicode('é')))
        ));
    }

    #[test]
    fn decodes_compact_binary_wheel_deltas_exactly() {
        let bytes = [
            BINARY_POINTER_WHEEL,
            0,
            0,
            0,
            0,
            0x22,
            0xc0,
            0x29,
            0x40,
            0x5e,
        ];
        let message = parse_binary_control(&bytes).unwrap();
        assert!(matches!(
            message,
            ControlMessage::Pointer {
                action: PointerAction::Wheel,
                delta_x: Some(x),
                delta_y: Some(y),
                ..
            } if x == -12.5 && y == 120.0
        ));
    }

    #[test]
    fn maps_function_and_system_keys() {
        assert!(matches!(map_key("F4", "F4"), Some(Key::F4)));
        assert!(matches!(map_key("F12", "F12"), Some(Key::F12)));
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(
            map_key("PrintScreen", "PrintScreen"),
            Some(Key::PrintScr)
        ));
    }
    #[cfg(target_os = "linux")]
    use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "run on a disposable X11 display with xvfb-run"]
    fn times_unicode_text_commit_on_x11() {
        for (label, text, bound) in [
            (
                "ASCII/Latin",
                "AbC xyz 123!? "
                    .chars()
                    .cycle()
                    .take(200)
                    .collect::<String>(),
                300,
            ),
            (
                "mixed Unicode",
                "Hello 日本語 café 🙂! ".chars().cycle().take(200).collect(),
                300,
            ),
            (
                "distinct CJK",
                (0x4e00..0x4ec8)
                    .map(|code| char::from_u32(code).unwrap())
                    .collect(),
                3000,
            ),
        ] {
            let mut input = InputController::new().unwrap();
            let started = Instant::now();
            input.enigo.text(&text).unwrap();
            println!("legacy 200-character {label}: {:?}", started.elapsed());
            drop(input);
            let mut input = InputController::new().unwrap();
            let (connection, _) = x11rb::connect(None).unwrap();
            x11rb::protocol::xkb::use_extension(&connection, 1, 0)
                .unwrap()
                .reply()
                .unwrap();
            let first = connection.setup().min_keycode;
            let count = connection.setup().max_keycode - first + 1;
            let before = connection
                .get_keyboard_mapping(first, count)
                .unwrap()
                .reply()
                .unwrap();
            let message =
                serde_json::to_vec(&serde_json::json!({"type":"text", "text":text})).unwrap();
            let started = Instant::now();
            input.dispatch(&message, InputLane::Reliable).unwrap();
            let elapsed = started.elapsed();
            println!("batched 200-character {label}: {elapsed:?}");
            let after = connection
                .get_keyboard_mapping(first, count)
                .unwrap()
                .reply()
                .unwrap();
            assert_eq!(before.keysyms_per_keycode, after.keysyms_per_keycode);
            assert_eq!(before.keysyms, after.keysyms);
            assert!(elapsed < Duration::from_millis(bound), "{elapsed:?}");
            if label == "ASCII/Latin" {
                input.enigo.key(Key::Control, Direction::Press).unwrap();
                let before = x11rb::protocol::xkb::get_state(&connection, 256)
                    .unwrap()
                    .reply()
                    .unwrap();
                input.dispatch(&message, InputLane::Reliable).unwrap();
                let after = x11rb::protocol::xkb::get_state(&connection, 256)
                    .unwrap()
                    .reply()
                    .unwrap();
                assert_eq!(before.base_mods, after.base_mods);
                input.enigo.key(Key::Control, Direction::Release).unwrap();
            }
        }
    }

    #[test]
    fn coordinates_are_bounded() {
        assert_eq!(normalized_pixel(-2.0, 1920), 0);
        assert_eq!(normalized_pixel(1.0, 1920), 1919);
        assert_eq!(normalized_pixel(f64::NAN, 1920), 0);
        assert_eq!(display_pixel(0.5, 1920, -1920), -960);
        assert_eq!(display_pixel(1.0, 1080, 1920), 2999);
    }

    #[test]
    fn wheel_deltas_are_bounded_and_precise() {
        assert_eq!(wheel_units(10_000.0), 2_400);
        assert_eq!(wheel_units(-10_000.0), -2_400);
        assert_eq!(wheel_units(f64::INFINITY), 0);
        assert_eq!(wheel_units(100.0), 120);
        assert_eq!(wheel_units(40.0), 48);
        assert_eq!(wheel_units(1.0), 1);
    }

    #[test]
    fn scroll_remainder_accrues_across_events() {
        let mut remainder = 0.0;
        assert_eq!(accrued_notches(&mut remainder, 60.0), 0);
        assert_eq!(accrued_notches(&mut remainder, 60.0), 1);
        assert!((remainder - 20.0).abs() < f64::EPSILON);
        assert_eq!(accrued_notches(&mut remainder, -260.0), -2);
        assert!((remainder + 40.0).abs() < f64::EPSILON);
        assert_eq!(accrued_notches(&mut remainder, f64::NAN), 0);
        remainder = 4_700.0;
        assert_eq!(accrued_notches(&mut remainder, 4_700.0), 48);
    }

    #[test]
    fn parses_release_and_keepalive_messages() {
        let text: ControlMessage =
            serde_json::from_slice(br#"{"type":"text","text":"commit"}"#).unwrap();
        assert!(matches!(text, ControlMessage::Text { text } if text == "commit"));
        let release: ControlMessage = serde_json::from_slice(br#"{"type":"release"}"#).unwrap();
        assert!(matches!(release, ControlMessage::Release));
        let ping: ControlMessage =
            serde_json::from_slice(br#"{"type":"ping","nonce":7,"sentAt":123}"#).unwrap();
        assert!(matches!(ping, ControlMessage::Ping));
        let display: ControlMessage =
            serde_json::from_slice(br#"{"type":"display","displayId":"42"}"#).unwrap();
        assert!(matches!(
            display,
            ControlMessage::Display { display_id } if display_id == "42"
        ));
    }

    proptest! {
        #[test]
        fn coordinate_and_scroll_math_is_total_and_bounded(
            coordinate in any::<f64>(),
            extent in any::<u32>(),
            origin in any::<i32>(),
            scroll in any::<f64>(),
        ) {
            let normalized = normalized_pixel(coordinate, extent);
            let upper = extent.saturating_sub(1).min(i32::MAX as u32) as i32;
            prop_assert!(normalized >= 0);
            prop_assert!(normalized <= upper);
            let displayed = display_pixel(coordinate, extent, origin);
            prop_assert!(displayed >= origin);
            prop_assert!(displayed <= origin.saturating_add(upper));
            prop_assert!((-2_400..=2_400).contains(&wheel_units(scroll)));
            let mut remainder = 0.0;
            prop_assert!((-48..=48).contains(&accrued_notches(&mut remainder, scroll)));
        }

        #[test]
        fn arbitrary_control_bytes_never_escape_the_bounded_parser(
            bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_CONTROL_MESSAGE_BYTES + 1),
        ) {
            if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
                prop_assert!(bytes.len() > MAX_CONTROL_MESSAGE_BYTES);
            } else {
                let _ = serde_json::from_slice::<ControlMessage>(&bytes);
            }
        }
    }
}
