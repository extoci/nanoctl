#[cfg(feature = "media")]
use std::time::{Duration, Instant};

#[cfg(feature = "media")]
use anyhow::Result;
use serde::Deserialize;

const MAX_PADS: usize = 4;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
struct Report(u16, u8, u8, i16, i16, i16, i16);

#[derive(Debug)]
pub struct Snapshot([Option<Report>; MAX_PADS]);

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut pads = [None; MAX_PADS];
        // Each entry is [slot, buttons, left trigger, right trigger, LX, LY, RX, RY].
        for (slot, buttons, lt, rt, lx, ly, rx, ry) in
            Vec::<(usize, u16, u8, u8, i16, i16, i16, i16)>::deserialize(deserializer)?
        {
            if slot >= MAX_PADS || pads[slot].is_some() || buttons & !0xf7ff != 0 {
                return Err(serde::de::Error::custom("invalid gamepad slot or buttons"));
            }
            pads[slot] = Some(Report(buttons, lt, rt, lx, ly, rx, ry));
        }
        Ok(Self(pads))
    }
}

#[cfg(feature = "media")]
trait VirtualPad: Send {
    fn update(&mut self, report: Report) -> Result<()>;
}

#[cfg(feature = "media")]
struct Pad {
    target: Box<dyn VirtualPad>,
    last: Option<Report>,
}

#[cfg(feature = "media")]
#[derive(Default)]
pub struct Gamepads {
    pads: [Option<Pad>; MAX_PADS],
    last_snapshot: Option<Instant>,
}

#[cfg(feature = "media")]
impl Gamepads {
    pub fn update(&mut self, snapshot: Snapshot) -> Result<()> {
        if !available() {
            return Ok(());
        }
        self.last_snapshot = Some(Instant::now());
        for (current, report) in self.pads.iter_mut().zip(snapshot.0) {
            if report.is_none() {
                *current = None;
            }
        }
        let mut error = None;
        for (slot, (current, report)) in self.pads.iter_mut().zip(snapshot.0).enumerate() {
            let Some(report) = report else {
                continue;
            };
            if let Err(err) = apply(current, slot, report) {
                *current = None;
                error = Some(err);
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub fn neutralize_if_idle(&mut self, timeout: Duration) {
        if self
            .last_snapshot
            .is_some_and(|last| last.elapsed() >= timeout)
        {
            self.last_snapshot = None;
            for current in &mut self.pads {
                if let Some(pad) = current {
                    if pad.target.update(Report::default()).is_err() {
                        *current = None;
                    } else {
                        pad.last = Some(Report::default());
                    }
                }
            }
        }
    }
}

#[cfg(feature = "media")]
fn apply(current: &mut Option<Pad>, slot: usize, report: Report) -> Result<()> {
    if current.is_none() {
        *current = Some(Pad {
            target: attach(slot)?,
            last: None,
        });
    }
    if let Some(pad) = current
        && pad.last != Some(report)
    {
        pad.target.update(report)?;
        pad.last = Some(report);
    }
    Ok(())
}

#[cfg(feature = "media")]
pub fn available() -> bool {
    #[cfg(debug_assertions)]
    if mock_path().is_some() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *AVAILABLE.get_or_init(|| match vigem_client::Client::connect() {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, "gamepad forwarding unavailable; check that ViGEmBus is installed");
                false
            }
        })
    }
    #[cfg(not(target_os = "windows"))]
    false
}

#[cfg(feature = "media")]
fn attach(_slot: usize) -> Result<Box<dyn VirtualPad>> {
    #[cfg(debug_assertions)]
    if let Some(path) = mock_path() {
        use std::io::Write;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(out, "{_slot} attached")?;
        return Ok(Box::new(MockPad(_slot, out)));
    }
    #[cfg(target_os = "windows")]
    {
        let client = vigem_client::Client::connect()?;
        let mut target =
            vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
        target.plugin()?;
        target.wait_ready()?;
        Ok(Box::new(target))
    }
    #[cfg(not(target_os = "windows"))]
    anyhow::bail!("gamepad forwarding unavailable")
}

#[cfg(all(feature = "media", target_os = "windows"))]
impl VirtualPad for vigem_client::Xbox360Wired<vigem_client::Client> {
    fn update(&mut self, Report(buttons, lt, rt, lx, ly, rx, ry): Report) -> Result<()> {
        self.update(&vigem_client::XGamepad {
            buttons: buttons.into(),
            left_trigger: lt,
            right_trigger: rt,
            thumb_lx: lx,
            thumb_ly: ly,
            thumb_rx: rx,
            thumb_ry: ry,
        })?;
        Ok(())
    }
}

#[cfg(all(feature = "media", debug_assertions))]
fn mock_path() -> Option<std::ffi::OsString> {
    crate::server::dev_user()?;
    std::env::var_os("NANOCTL_DEV_GAMEPAD_REPORTS")
}

#[cfg(all(feature = "media", debug_assertions))]
struct MockPad(usize, std::fs::File);

#[cfg(all(feature = "media", debug_assertions))]
impl VirtualPad for MockPad {
    fn update(&mut self, Report(buttons, lt, rt, lx, ly, rx, ry): Report) -> Result<()> {
        use std::io::Write;
        writeln!(self.1, "{} {buttons} {lt} {rt} {lx} {ly} {rx} {ry}", self.0)?;
        Ok(())
    }
}

#[cfg(all(feature = "media", debug_assertions))]
impl Drop for MockPad {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = writeln!(self.1, "{} detached", self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_quantized_reports_by_slot() {
        let report: Snapshot = serde_json::from_str("[[2,4096,64,255,-32768,32767,0,1]]").unwrap();
        assert_eq!(
            report.0,
            [
                None,
                None,
                Some(Report(4096, 64, 255, -32768, 32767, 0, 1)),
                None
            ]
        );
        let empty: Snapshot = serde_json::from_str("[]").unwrap();
        assert_eq!(empty.0, [None; MAX_PADS]);
    }

    #[test]
    fn rejects_invalid_reports() {
        for invalid in [
            "[[4,0,0,0,0,0,0,0]]",
            "[[0,0,0,0,0,0,0,0],[0,0,0,0,0,0,0,0]]",
            "[[0,2048,0,0,0,0,0,0]]",
            "[[0,0,256,0,0,0,0,0]]",
            "[[0,0,0,0,-32769,0,0,0]]",
            "[[0,0,0,0,0,0,0]]",
        ] {
            assert!(
                serde_json::from_str::<Snapshot>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}
