use std::collections::HashSet;
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

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    fn send(input: Input) -> Result<()> {
        let sent = unsafe { SendInput(1, &input, std::mem::size_of::<Input>() as i32) };
        if sent != 1 {
            return Err(enigo::InputError::Simulate("SendInput rejected the mouse event").into());
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
        send(event(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            0,
            nx,
            ny,
        ))
    }

    /// One wheel event in raw wheel-delta units (120 = one notch).
    pub fn wheel(units: i32, horizontal: bool) -> Result<()> {
        let (flags, data) = if horizontal {
            (MOUSEEVENTF_HWHEEL, units)
        } else {
            // Positive mouseData scrolls up; viewer deltas are down-positive.
            (MOUSEEVENTF_WHEEL, -units)
        };
        send(event(flags, data, 0, 0))
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

pub struct InputController {
    enigo: Enigo,
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    held_keys: HashSet<Key>,
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

impl InputController {
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
        let message: ControlMessage =
            serde_json::from_slice(bytes).context("control message is invalid")?;
        match (&message, lane) {
            (
                ControlMessage::Pointer {
                    action: PointerAction::Move,
                    ..
                },
                InputLane::PointerMotion,
            )
            | (ControlMessage::Gamepad(_), InputLane::PointerMotion) => {}
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
                anyhow::bail!("only pointer motion and gamepad reports are accepted on the pointer channel");
            }
            (_, InputLane::Reliable) => {}
        }
        if !matches!(
            &message,
            ControlMessage::Ping
                | ControlMessage::Release
                | ControlMessage::Gamepad(_)
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
            PointerAction::Move => self.move_to(x, y)?,
            PointerAction::Down => {
                self.move_to(x, y)?;
                let button = map_button(button)?;
                self.enigo.button(button, Direction::Press)?;
                self.held_buttons.insert(button);
            }
            PointerAction::Up => {
                self.move_to(x, y)?;
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
        let Some(key) = map_key(code, value) else {
            return Ok(());
        };
        match action {
            KeyAction::Down if repeat || !self.held_keys.contains(&key) => {
                self.enigo.key(key, Direction::Press)?;
                self.held_keys.insert(key);
            }
            KeyAction::Up => {
                self.held_keys.remove(&key);
                self.enigo.key(key, Direction::Release)?;
            }
            KeyAction::Down => {}
        }
        Ok(())
    }

    pub fn release_all(&mut self) {
        for key in self.held_keys.drain() {
            let _ = self.enigo.key(key, Direction::Release);
        }
        for button in self.held_buttons.drain() {
            let _ = self.enigo.button(button, Direction::Release);
        }
    }

    pub fn release_if_idle(&mut self, timeout: Duration) {
        self.gamepads.neutralize_if_idle(timeout);
        if self.last_physical_input.elapsed() >= timeout
            && (!self.held_keys.is_empty() || !self.held_buttons.is_empty())
        {
            self.release_all();
        }
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
    use crate::rtc::MAX_CONTROL_MESSAGE_BYTES;
    use proptest::prelude::*;

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
