#[cfg(feature = "media")]
use std::time::{Duration, Instant};

#[cfg(feature = "media")]
use anyhow::Result;
use serde::Deserialize;

const MAX_PADS: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Report(u16, u8, u8, i16, i16, i16, i16);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Ds4Buttons {
    left_trigger_pressed: bool,
    right_trigger_pressed: bool,
    touchpad_pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PadKind {
    Xbox360,
    DualShock4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PadReport {
    state: Report,
    ds4: Option<Ds4Buttons>,
}

impl PadReport {
    fn kind(self) -> PadKind {
        if self.ds4.is_some() {
            PadKind::DualShock4
        } else {
            PadKind::Xbox360
        }
    }

    fn neutral(kind: PadKind) -> Self {
        Self {
            state: Report::default(),
            ds4: (kind == PadKind::DualShock4).then(Ds4Buttons::default),
        }
    }
}

#[derive(Deserialize)]
struct WireDs4Pad {
    slot: usize,
    #[serde(flatten)]
    buttons: Ds4Buttons,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSnapshot {
    pads: Vec<WirePad>,
    #[serde(default)]
    dual_shock4: Vec<WireDs4Pad>,
}

// Each entry is [slot, buttons, left trigger, right trigger, LX, LY, RX, RY].
#[derive(Deserialize)]
struct WirePad(usize, u16, u8, u8, i16, i16, i16, i16);

#[derive(Debug, Deserialize)]
#[serde(try_from = "WireSnapshot")]
pub struct Snapshot([Option<PadReport>; MAX_PADS]);

impl TryFrom<WireSnapshot> for Snapshot {
    type Error = &'static str;

    fn try_from(wire: WireSnapshot) -> Result<Self, Self::Error> {
        let mut pads = [None; MAX_PADS];
        for WirePad(slot, buttons, lt, rt, lx, ly, rx, ry) in wire.pads {
            if slot >= MAX_PADS || pads[slot].is_some() || buttons & !0xf7ff != 0 {
                return Err("invalid gamepad slot or buttons");
            }
            pads[slot] = Some(PadReport {
                state: Report(buttons, lt, rt, lx, ly, rx, ry),
                ds4: None,
            });
        }
        for entry in wire.dual_shock4 {
            let Some(report) = pads.get_mut(entry.slot).and_then(Option::as_mut) else {
                return Err("invalid DualShock 4 slot");
            };
            if report.ds4.replace(entry.buttons).is_some() {
                return Err("duplicate DualShock 4 slot");
            }
        }
        Ok(Self(pads))
    }
}

#[cfg(feature = "media")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateStatus {
    Applied,
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    Retry,
}

#[cfg(feature = "media")]
trait VirtualPad: Send {
    fn update(&mut self, report: PadReport) -> Result<UpdateStatus>;
}

#[cfg(feature = "media")]
struct Pad {
    kind: PadKind,
    target: Box<dyn VirtualPad>,
    last: Option<PadReport>,
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
            if current.as_ref().map(|pad| pad.kind) != report.map(PadReport::kind) {
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
                    let neutral = PadReport::neutral(pad.kind);
                    match pad.target.update(neutral) {
                        Ok(UpdateStatus::Applied) => pad.last = Some(neutral),
                        Ok(UpdateStatus::Retry) => {}
                        Err(_) => *current = None,
                    }
                }
            }
        }
    }
}

#[cfg(feature = "media")]
fn apply(current: &mut Option<Pad>, slot: usize, report: PadReport) -> Result<()> {
    if current.is_none() {
        *current = Some(Pad {
            kind: report.kind(),
            target: attach(slot, report.kind())?,
            last: None,
        });
    }
    if let Some(pad) = current
        && pad.last != Some(report)
        && pad.target.update(report)? == UpdateStatus::Applied
    {
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
fn attach(_slot: usize, _kind: PadKind) -> Result<Box<dyn VirtualPad>> {
    #[cfg(debug_assertions)]
    if let Some(path) = mock_path() {
        use std::io::Write;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let kind = match _kind {
            PadKind::Xbox360 => "xbox360",
            PadKind::DualShock4 => "dualShock4",
        };
        writeln!(out, "{_slot} attached {kind}")?;
        return Ok(Box::new(MockPad(_slot, out)));
    }
    #[cfg(target_os = "windows")]
    {
        use anyhow::Context;

        let client = vigem_client::Client::connect().context("ViGEmBus connect")?;
        match _kind {
            PadKind::Xbox360 => {
                let mut target =
                    vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
                target.plugin().context("ViGEmBus Xbox 360 plugin")?;
                target
                    .wait_ready()
                    .context("ViGEmBus Xbox 360 wait_ready")?;
                Ok(Box::new(target))
            }
            PadKind::DualShock4 => {
                let mut target = vigem_client::DualShock4Wired::new(
                    client,
                    vigem_client::TargetId::DUALSHOCK4_WIRED,
                );
                target.plugin().context("ViGEmBus DualShock 4 plugin")?;
                target
                    .wait_ready()
                    .context("ViGEmBus DualShock 4 wait_ready")?;
                Ok(Box::new(target))
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    anyhow::bail!("gamepad forwarding unavailable")
}

// ViGEmBus returns STATUS_NO_MORE_ENTRIES (Win32 ERROR_NO_MORE_ITEMS = 259)
// when a report is submitted before Windows has queued a pending HID read IRP.
#[cfg(all(feature = "media", target_os = "windows"))]
const ERROR_NO_MORE_ITEMS: u32 = 259;

#[cfg(all(feature = "media", target_os = "windows"))]
fn map_vigem_update(result: Result<(), vigem_client::Error>) -> Result<UpdateStatus> {
    match result {
        Ok(()) => Ok(UpdateStatus::Applied),
        Err(
            vigem_client::Error::WinError(ERROR_NO_MORE_ITEMS)
            | vigem_client::Error::TargetNotReady,
        ) => Ok(UpdateStatus::Retry),
        Err(error) => Err(error.into()),
    }
}

#[cfg(all(feature = "media", target_os = "windows"))]
impl VirtualPad for vigem_client::Xbox360Wired<vigem_client::Client> {
    fn update(&mut self, report: PadReport) -> Result<UpdateStatus> {
        let Report(buttons, lt, rt, lx, ly, rx, ry) = report.state;
        map_vigem_update(self.update(&vigem_client::XGamepad {
            buttons: buttons.into(),
            left_trigger: lt,
            right_trigger: rt,
            thumb_lx: lx,
            thumb_ly: ly,
            thumb_rx: rx,
            thumb_ry: ry,
        }))
    }
}

#[cfg(any(test, target_os = "windows", debug_assertions))]
#[derive(Debug, PartialEq, Eq)]
struct Ds4Report {
    sticks: [u8; 4],
    buttons: u16,
    special: u8,
    triggers: [u8; 2],
}

#[cfg(any(test, target_os = "windows", debug_assertions))]
fn encode_ds4(Report(buttons, lt, rt, lx, ly, rx, ry): Report, ds4: Ds4Buttons) -> Ds4Report {
    let x = i8::from(buttons & 0x8 != 0) - i8::from(buttons & 0x4 != 0);
    let y = i8::from(buttons & 0x2 != 0) - i8::from(buttons & 0x1 != 0);
    let hat = match (x, y) {
        (0, -1) => 0,
        (1, -1) => 1,
        (1, 0) => 2,
        (1, 1) => 3,
        (0, 1) => 4,
        (-1, 1) => 5,
        (-1, 0) => 6,
        (-1, -1) => 7,
        _ => 8,
    };
    let mut mapped = hat | (buttons & 0x0300);
    for (from, to) in [
        (0x1000, 0x0020), // A -> Cross
        (0x2000, 0x0040), // B -> Circle
        (0x4000, 0x0010), // X -> Square
        (0x8000, 0x0080), // Y -> Triangle
        (0x0020, 0x1000), // Back -> Share
        (0x0010, 0x2000), // Start -> Options
        (0x0040, 0x4000), // L3
        (0x0080, 0x8000), // R3
    ] {
        if buttons & from != 0 {
            mapped |= to;
        }
    }
    let axis = |value: i16| ((i32::from(value) + 32768) >> 8) as u8;
    Ds4Report {
        sticks: [
            axis(lx),
            axis(ly.saturating_neg()),
            axis(rx),
            axis(ry.saturating_neg()),
        ],
        buttons: mapped
            | (u16::from(ds4.left_trigger_pressed) << 10)
            | (u16::from(ds4.right_trigger_pressed) << 11),
        special: u8::from(buttons & 0x0400 != 0) | (u8::from(ds4.touchpad_pressed) << 1),
        triggers: [lt, rt],
    }
}

#[cfg(all(feature = "media", target_os = "windows"))]
impl VirtualPad for vigem_client::DualShock4Wired<vigem_client::Client> {
    fn update(&mut self, report: PadReport) -> Result<UpdateStatus> {
        let encoded = encode_ds4(report.state, report.ds4.unwrap_or_default());
        let [thumb_lx, thumb_ly, thumb_rx, thumb_ry] = encoded.sticks;
        let [trigger_l, trigger_r] = encoded.triggers;
        map_vigem_update(self.update(&vigem_client::DS4Report {
            thumb_lx,
            thumb_ly,
            thumb_rx,
            thumb_ry,
            buttons: encoded.buttons,
            special: encoded.special,
            trigger_l,
            trigger_r,
        }))
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
    fn update(&mut self, report: PadReport) -> Result<UpdateStatus> {
        use std::io::Write;
        if let Some(ds4) = report.ds4 {
            let encoded = encode_ds4(report.state, ds4);
            let [lx, ly, rx, ry] = encoded.sticks;
            let [lt, rt] = encoded.triggers;
            writeln!(
                self.1,
                "{} dualShock4 {} {} {lt} {rt} {lx} {ly} {rx} {ry}",
                self.0, encoded.buttons, encoded.special
            )?;
        } else {
            let Report(buttons, lt, rt, lx, ly, rx, ry) = report.state;
            writeln!(self.1, "{} {buttons} {lt} {rt} {lx} {ly} {rx} {ry}", self.0)?;
        }
        Ok(UpdateStatus::Applied)
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

    fn snapshot(pads: &str) -> serde_json::Result<Snapshot> {
        serde_json::from_str(&format!(r#"{{"pads":{pads}}}"#))
    }

    #[test]
    fn decodes_quantized_reports_by_slot() {
        let report = snapshot("[[2,4096,64,255,-32768,32767,0,1]]").unwrap();
        assert_eq!(
            report.0,
            [
                None,
                None,
                Some(PadReport {
                    state: Report(4096, 64, 255, -32768, 32767, 0, 1),
                    ds4: None,
                }),
                None
            ]
        );
        let empty = snapshot("[]").unwrap();
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
            assert!(snapshot(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn validates_playstation_metadata_by_slot() {
        let input = |entries: &[usize]| {
            serde_json::from_value::<Snapshot>(serde_json::json!({
                "pads": [[0,0,0,0,0,0,0,0], [2,0,0,0,0,0,0,0]],
                "dualShock4": entries.iter().map(|slot| serde_json::json!({
                    "slot": slot,
                    "leftTriggerPressed": true,
                    "rightTriggerPressed": false,
                    "touchpadPressed": true,
                })).collect::<Vec<_>>(),
            }))
        };
        let report = input(&[2]).unwrap();
        assert_eq!(report.0[0].unwrap().kind(), PadKind::Xbox360);
        assert_eq!(report.0[2].unwrap().kind(), PadKind::DualShock4);
        assert_eq!(
            report.0[2].unwrap().ds4,
            Some(Ds4Buttons {
                left_trigger_pressed: true,
                right_trigger_pressed: false,
                touchpad_pressed: true,
            })
        );
        for invalid in [&[1][..], &[4], &[2, 2]] {
            assert!(input(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn encodes_dualshock_buttons_and_axes() {
        let buttons = Ds4Buttons {
            left_trigger_pressed: false,
            right_trigger_pressed: true,
            touchpad_pressed: true,
        };
        assert_eq!(
            encode_ds4(
                Report(0xf7f9, 64, 211, -32768, 32767, 32767, -32768),
                buttons
            ),
            Ds4Report {
                sticks: [0, 0, 255, 255],
                buttons: 0xfbf1,
                special: 3,
                triggers: [64, 211],
            }
        );
        let neutral = PadReport::neutral(PadKind::DualShock4);
        assert_eq!(
            encode_ds4(neutral.state, neutral.ds4.unwrap()),
            Ds4Report {
                sticks: [128; 4],
                buttons: 8,
                special: 0,
                triggers: [0; 2],
            }
        );
    }

    #[test]
    fn encodes_dualshock_dpad_as_a_hat() {
        for (mask, expected) in [
            (1, 0),
            (9, 1),
            (8, 2),
            (10, 3),
            (2, 4),
            (6, 5),
            (4, 6),
            (5, 7),
            (15, 8),
        ] {
            assert_eq!(
                encode_ds4(Report(mask, 0, 0, 0, 0, 0, 0), Ds4Buttons::default()).buttons,
                expected
            );
        }
    }

    #[cfg(feature = "media")]
    mod media_tests {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};

        use super::*;

        struct ScriptedPad {
            outcomes: VecDeque<Result<UpdateStatus>>,
            updates: Arc<Mutex<Vec<PadReport>>>,
        }

        impl VirtualPad for ScriptedPad {
            fn update(&mut self, report: PadReport) -> Result<UpdateStatus> {
                self.updates.lock().unwrap().push(report);
                self.outcomes
                    .pop_front()
                    .unwrap_or(Ok(UpdateStatus::Applied))
            }
        }

        #[test]
        fn transient_retry_keeps_pad_attached_and_retries_same_report() {
            let updates = Arc::new(Mutex::new(Vec::new()));
            let mut slot = Some(Pad {
                kind: PadKind::DualShock4,
                target: Box::new(ScriptedPad {
                    outcomes: VecDeque::from([Ok(UpdateStatus::Retry), Ok(UpdateStatus::Applied)]),
                    updates: Arc::clone(&updates),
                }),
                last: None,
            });
            let report = PadReport {
                state: Report(0x1000, 128, 0, 100, -100, 0, 0),
                ds4: Some(Ds4Buttons::default()),
            };

            apply(&mut slot, 0, report).unwrap();
            assert!(slot.is_some());
            assert_eq!(slot.as_ref().unwrap().last, None);

            apply(&mut slot, 0, report).unwrap();
            assert_eq!(slot.as_ref().unwrap().last, Some(report));

            // Subsequent identical snapshot is deduplicated once applied.
            apply(&mut slot, 0, report).unwrap();
            assert_eq!(updates.lock().unwrap().as_slice(), &[report, report]);
        }

        #[test]
        fn neutralize_if_idle_keeps_pad_on_retry_and_drops_on_fatal_error() {
            let updates = Arc::new(Mutex::new(Vec::new()));
            let mut gamepads = Gamepads::default();
            gamepads.pads[0] = Some(Pad {
                kind: PadKind::Xbox360,
                target: Box::new(ScriptedPad {
                    outcomes: VecDeque::from([
                        Ok(UpdateStatus::Retry),
                        Err(anyhow::anyhow!("bus disconnected")),
                    ]),
                    updates: Arc::clone(&updates),
                }),
                last: None,
            });

            gamepads.last_snapshot = Some(Instant::now() - Duration::from_secs(5));
            gamepads.neutralize_if_idle(Duration::from_secs(1));
            assert!(gamepads.pads[0].is_some());
            assert_eq!(gamepads.pads[0].as_ref().unwrap().last, None);

            gamepads.last_snapshot = Some(Instant::now() - Duration::from_secs(5));
            gamepads.neutralize_if_idle(Duration::from_secs(1));
            assert!(gamepads.pads[0].is_none());
        }
    }
}
