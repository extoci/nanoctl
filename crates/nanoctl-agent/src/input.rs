use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde::Deserialize;
use xcap::Monitor;

const MAX_CONTROL_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub enum InputLane {
    Reliable,
    PointerMotion,
}

pub struct InputController {
    enigo: Enigo,
    width: u32,
    height: u32,
    held_keys: HashSet<Key>,
    held_buttons: HashSet<Button>,
    last_activity: Instant,
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
    Ping,
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
            width: monitor.width()?,
            height: monitor.height()?,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            last_activity: Instant::now(),
        })
    }

    pub fn dispatch(&mut self, bytes: &[u8], lane: InputLane) -> Result<()> {
        if bytes.len() > MAX_CONTROL_BYTES {
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
            | (
                ControlMessage::Pointer {
                    action: PointerAction::Move,
                    ..
                },
                InputLane::Reliable,
            ) => {
                if matches!(lane, InputLane::Reliable) {
                    anyhow::bail!("pointer motion must use the pointer channel");
                }
            }
            (_, InputLane::PointerMotion) => {
                anyhow::bail!("only pointer motion is accepted on the pointer channel");
            }
            (_, InputLane::Reliable) => {}
        }
        self.last_activity = Instant::now();
        match message {
            ControlMessage::Pointer {
                action,
                x,
                y,
                button,
                delta_x,
                delta_y,
            } => self.pointer(action, x, y, button, delta_x, delta_y),
            ControlMessage::Key {
                action,
                code,
                key,
                repeat,
            } => self.keyboard(action, &code, &key, repeat),
            ControlMessage::Release => {
                self.release_all();
                Ok(())
            }
            ControlMessage::Ping => Ok(()),
            ControlMessage::Unsupported => Ok(()),
        }
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
        let x = normalized_pixel(x, self.width);
        let y = normalized_pixel(y, self.height);
        match action {
            PointerAction::Move => self.enigo.move_mouse(x, y, Coordinate::Abs)?,
            PointerAction::Down => {
                self.enigo.move_mouse(x, y, Coordinate::Abs)?;
                let button = map_button(button)?;
                self.enigo.button(button, Direction::Press)?;
                self.held_buttons.insert(button);
            }
            PointerAction::Up => {
                self.enigo.move_mouse(x, y, Coordinate::Abs)?;
                let button = map_button(button)?;
                self.enigo.button(button, Direction::Release)?;
                self.held_buttons.remove(&button);
            }
            PointerAction::Wheel => {
                let horizontal = bounded_scroll(delta_x.unwrap_or(0.0));
                let vertical = bounded_scroll(delta_y.unwrap_or(0.0));
                if horizontal != 0 {
                    self.enigo.scroll(horizontal, Axis::Horizontal)?;
                }
                if vertical != 0 {
                    self.enigo.scroll(vertical, Axis::Vertical)?;
                }
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
            KeyAction::Down if repeat || self.held_keys.insert(key) => {
                self.enigo.key(key, Direction::Press)?;
            }
            KeyAction::Up => {
                self.enigo.key(key, Direction::Release)?;
                self.held_keys.remove(&key);
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
        if self.last_activity.elapsed() >= timeout
            && (!self.held_keys.is_empty() || !self.held_buttons.is_empty())
        {
            self.release_all();
        }
    }
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

fn bounded_scroll(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    (value / 100.0).round().clamp(-20.0, 20.0) as i32
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
    fn coordinates_are_bounded() {
        assert_eq!(normalized_pixel(-2.0, 1920), 0);
        assert_eq!(normalized_pixel(1.0, 1920), 1919);
        assert_eq!(normalized_pixel(f64::NAN, 1920), 0);
    }

    #[test]
    fn scroll_is_bounded() {
        assert_eq!(bounded_scroll(10_000.0), 20);
        assert_eq!(bounded_scroll(-10_000.0), -20);
        assert_eq!(bounded_scroll(f64::INFINITY), 0);
    }

    #[test]
    fn parses_release_and_keepalive_messages() {
        let release: ControlMessage = serde_json::from_slice(br#"{"type":"release"}"#).unwrap();
        assert!(matches!(release, ControlMessage::Release));
        let ping: ControlMessage =
            serde_json::from_slice(br#"{"type":"ping","nonce":7,"sentAt":123}"#).unwrap();
        assert!(matches!(ping, ControlMessage::Ping));
    }
}
