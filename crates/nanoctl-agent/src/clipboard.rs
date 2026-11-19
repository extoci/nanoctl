use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use tokio::sync::watch;

#[derive(Clone)]
pub struct Backend {
    sets: mpsc::SyncSender<String>,
    pub changes: watch::Receiver<Option<String>>,
    current: watch::Receiver<Option<String>>,
}

pub fn backend() -> Option<Backend> {
    static BACKEND: OnceLock<Option<Backend>> = OnceLock::new();
    BACKEND.get_or_init(start).clone()
}

pub fn parse(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= crate::rtc::MAX_CONTROL_MESSAGE_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    (value.get("type")?.as_str()? == "clipboard")
        .then(|| value.get("text")?.as_str().map(str::to_owned))?
}

pub fn encode(text: &str) -> Option<String> {
    let message = serde_json::json!({"type":"clipboard", "text":text}).to_string();
    (message.len() < crate::rtc::MAX_CONTROL_MESSAGE_BYTES).then_some(message)
}

#[derive(Default)]
struct State {
    last: Option<String>,
}

impl State {
    fn changed(&mut self, text: String) -> Option<String> {
        if self.last.as_ref() == Some(&text) {
            return None;
        }
        self.last = Some(text.clone());
        if let Some(message) = encode(&text) {
            Some(message)
        } else {
            tracing::warn!("skipping oversized host clipboard text");
            None
        }
    }
}

impl Backend {
    pub fn current(&self) -> Option<String> {
        self.current.borrow().clone()
    }

    pub fn set(&self, text: String) {
        let _ = self.sets.try_send(text);
    }
}

fn start() -> Option<Backend> {
    let (sets, requests) = mpsc::sync_channel::<String>(16);
    let (changes, receiver) = watch::channel(None);
    let (current, snapshot) = watch::channel(None);
    let (ready, opened) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                let _ = ready.send(false);
                return;
            };
            let mut state = State::default();
            if let Ok(text) = clipboard.get_text() {
                current.send_replace(encode(&text));
                changes.send_replace(state.changed(text));
            }
            let _ = ready.send(true);
            loop {
                match requests.recv_timeout(Duration::from_millis(350)) {
                    Ok(text) => {
                        if clipboard.set_text(text.clone()).is_ok() {
                            current.send_replace(encode(&text));
                            state.last = Some(text);
                            changes.send_replace(None);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if let Ok(text) = clipboard.get_text() {
                    current.send_replace(encode(&text));
                    if let Some(message) = state.changed(text) {
                        changes.send_replace(Some(message));
                    }
                }
            }
        })
        .ok()?;
    opened.recv().ok()?.then_some(Backend {
        sets,
        changes: receiver,
        current: snapshot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_limits() {
        for text in ["", "hello", "\"\n🦀"] {
            assert_eq!(
                parse(encode(text).unwrap().as_bytes()).as_deref(),
                Some(text)
            );
        }
        for invalid in [
            r#"{"type":"clipboard","text":3}"#,
            "null",
            "bad",
            r#"{"type":"key","text":"a"}"#,
        ] {
            assert!(parse(invalid.as_bytes()).is_none());
        }
        assert!(encode(&"\n".repeat(65536)).is_none());
        assert!(parse(&vec![b' '; 65536]).is_none());
    }

    #[test]
    fn suppresses_repeated_and_viewer_text() {
        let mut state = State::default();
        assert!(state.changed("host".into()).is_some());
        assert!(state.changed("host".into()).is_none());
        state.last = Some("viewer".into());
        assert!(state.changed("viewer".into()).is_none());
        assert!(state.changed(String::new()).is_some());
    }

    #[test]
    #[ignore = "requires a desktop clipboard; DISPLAY=:0 cargo test --features media manual_x_clipboard -- --ignored --nocapture"]
    fn manual_x_clipboard() {
        let mut clipboard = arboard::Clipboard::new().unwrap();
        let previous = clipboard.get_text().ok();
        let text = format!("nanoctl clipboard check {}", std::process::id());
        clipboard.set_text(text.clone()).unwrap();
        assert_eq!(clipboard.get_text().unwrap(), text);
        println!("arboard X clipboard text write/read succeeded");
        if let Some(previous) = previous {
            clipboard.set_text(previous).unwrap();
        }
    }
}
