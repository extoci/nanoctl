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
pub struct Snapshot([Option<PadReport>; MAX_PADS], bool, u8);

impl Snapshot {
    /// Parses wire-v3 gamepad state: u8 sequence, u4 slot mask, then 13 bytes
    /// for each masked slot in ascending order — u16 buttons, u8 LT/RT,
    /// i16 LX/LY/RX/RY, and u8 DualShock flags. In sparse reports masked slots
    /// are absolute replacement state and omitted slots are unchanged.
    pub(crate) fn parse_binary(data: &[u8], sparse: bool) -> Result<Self, &'static str> {
        if data.len() < 2 {
            return Err("truncated gamepad report");
        }
        let sequence = data[0];
        let mask = data[1];
        if mask & !0x0f != 0 {
            return Err("invalid gamepad slot mask");
        }
        let count = mask.count_ones() as usize;
        let mut pads = Vec::with_capacity(count);
        let mut dual_shock4 = Vec::new();
        let mut offset = 2;
        if !sparse && data.len() - offset != count * 13 {
            return Err("invalid gamepad report length");
        }
        for slot in 0..MAX_PADS {
            if mask & (1 << slot) == 0 {
                continue;
            }
            let (buttons, left_trigger, right_trigger, axes, ds4_flags) = if sparse {
                let Some(&fields) = data.get(offset) else {
                    return Err("truncated sparse gamepad report");
                };
                offset += 1;
                let take = |offset: &mut usize, len: usize| -> Result<&[u8], &'static str> {
                    let end = offset
                        .checked_add(len)
                        .ok_or("invalid sparse gamepad length")?;
                    let bytes = data
                        .get(*offset..end)
                        .ok_or("truncated sparse gamepad report")?;
                    *offset = end;
                    Ok(bytes)
                };
                let mut buttons = 0;
                let mut triggers = [0, 0];
                let mut axes = [0, 0, 0, 0];
                let mut ds4_flags = None;
                if fields & 1 != 0 {
                    let mut value = 0_u32;
                    for shift in (0..=21).step_by(7) {
                        let byte = take(&mut offset, 1)?[0];
                        value |= u32::from(byte & 0x7f) << shift;
                        if byte & 0x80 == 0 {
                            buttons = u16::try_from(value)
                                .map_err(|_| "invalid sparse gamepad buttons")?;
                            if buttons == 0 {
                                return Err("invalid sparse gamepad buttons");
                            }
                            break;
                        }
                        if shift == 21 {
                            return Err("invalid sparse gamepad buttons");
                        }
                    }
                }
                if fields & 2 != 0 {
                    triggers[0] = take(&mut offset, 1)?[0];
                    if triggers[0] == 0 {
                        return Err("invalid sparse gamepad trigger");
                    }
                }
                if fields & 4 != 0 {
                    triggers[1] = take(&mut offset, 1)?[0];
                    if triggers[1] == 0 {
                        return Err("invalid sparse gamepad trigger");
                    }
                }
                for (bit, axis) in axes.iter_mut().enumerate() {
                    if fields & (8 << bit) != 0 {
                        let bytes = take(&mut offset, 2)?;
                        *axis = i16::from_le_bytes([bytes[0], bytes[1]]);
                        if *axis == 0 {
                            return Err("invalid sparse gamepad axis");
                        }
                    }
                }
                if fields & 128 != 0 {
                    ds4_flags = Some(take(&mut offset, 1)?[0]);
                }
                (buttons, triggers[0], triggers[1], axes, ds4_flags)
            } else {
                let entry = &data[offset..offset + 13];
                offset += 13;
                if entry[12] & !0x0f != 0 {
                    return Err("invalid DualShock 4 flags");
                }
                let axis = |index: usize| i16::from_le_bytes([entry[index], entry[index + 1]]);
                (
                    u16::from_le_bytes([entry[0], entry[1]]),
                    entry[2],
                    entry[3],
                    [axis(4), axis(6), axis(8), axis(10)],
                    (entry[12] & 1 != 0).then_some(entry[12]),
                )
            };
            pads.push(WirePad(
                slot,
                buttons,
                left_trigger,
                right_trigger,
                axes[0],
                axes[1],
                axes[2],
                axes[3],
            ));
            if let Some(flags) = ds4_flags {
                if flags & !0x0f != 0 || flags & 1 == 0 {
                    return Err("invalid DualShock 4 flags");
                }
                dual_shock4.push(WireDs4Pad {
                    slot,
                    buttons: Ds4Buttons {
                        left_trigger_pressed: flags & 0x02 != 0,
                        right_trigger_pressed: flags & 0x04 != 0,
                        touchpad_pressed: flags & 0x08 != 0,
                    },
                });
            }
        }
        if offset != data.len() {
            return Err("invalid gamepad report length");
        }
        let mut snapshot = Snapshot::try_from(WireSnapshot { pads, dual_shock4 })?;
        snapshot.1 = sparse;
        snapshot.2 = sequence;
        Ok(snapshot)
    }
}

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
        Ok(Self(pads, false, 0))
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
    sequence: Option<u8>,
    waiting_for_full: bool,
}

#[cfg(feature = "media")]
impl Gamepads {
    pub fn update(&mut self, snapshot: Snapshot) -> Result<()> {
        let sparse = snapshot.1;
        let sequence = snapshot.2;
        if let Some(last) = self.sequence {
            let newer = sequence != last && sequence.wrapping_sub(last) < 0x80;
            if !newer {
                return Ok(());
            }
        }
        let missing_base = self.sequence.is_none() || self.waiting_for_full;
        if sparse && missing_base {
            // Sparse records are absolute per-slot state, and the sender
            // accumulates every slot differing from the receiver's last full
            // baseline. Before that baseline arrives, though, an omitted slot
            // could hide state the host never received.
            self.sequence = Some(sequence);
            self.waiting_for_full = true;
            return Ok(());
        }
        self.sequence = Some(sequence);
        if !sparse {
            self.waiting_for_full = false;
        }
        if !available() {
            return Ok(());
        }
        self.last_snapshot = Some(Instant::now());
        for (current, report) in self.pads.iter_mut().zip(snapshot.0) {
            let detach = !sparse && report.is_none();
            if (detach || report.is_some())
                && current.as_ref().map(|pad| pad.kind) != report.map(PadReport::kind)
            {
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

    /// A heartbeat proves the viewer is alive but carries no replacement
    /// state. Once pads have been neutralized it must not resurrect them.
    pub fn heartbeat(&mut self) {
        if self.last_snapshot.is_some() {
            self.last_snapshot = Some(Instant::now());
        }
    }

    /// Zero every attached pad without unplugging it, so a transient channel
    /// loss does not look like the controller was removed.
    pub fn neutralize(&mut self) {
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

    pub fn neutralize_if_idle(&mut self, timeout: Duration) {
        if self
            .last_snapshot
            .is_some_and(|last| last.elapsed() >= timeout)
        {
            self.neutralize();
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
    fn decodes_binary_reports_by_slot() {
        let report = Snapshot::parse_binary(
            &[
                7, 4, 0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
            ],
            false,
        )
        .unwrap();
        assert_eq!(report.2, 7);
        assert_eq!(
            report.0[2],
            Some(PadReport {
                state: Report(0x1001, 7, 8, -32768, 32767, -1, 1),
                ds4: Some(Ds4Buttons {
                    left_trigger_pressed: true,
                    right_trigger_pressed: false,
                    touchpad_pressed: true,
                }),
            })
        );
        assert!(
            Snapshot::parse_binary(&[0, 0], false)
                .unwrap()
                .0
                .iter()
                .all(Option::is_none)
        );
        for invalid in [
            &[][..],
            &[1],
            &[1, 0x10],
            &[1, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10],
        ] {
            assert!(
                Snapshot::parse_binary(invalid, false).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn decodes_sparse_binary_reports_with_zero_fields_omitted() {
        let report = Snapshot::parse_binary(
            &[
                9, 4, 0xff, 0x81, 0x20, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
            ],
            true,
        )
        .unwrap();
        assert_eq!(report.2, 9);
        assert!(report.1);
        assert_eq!(
            report.0[2],
            Some(PadReport {
                state: Report(0x1001, 7, 8, -32768, 32767, -1, 1),
                ds4: Some(Ds4Buttons {
                    left_trigger_pressed: true,
                    right_trigger_pressed: false,
                    touchpad_pressed: true,
                }),
            })
        );

        let report = Snapshot::parse_binary(&[10, 2, 12, 7, 0xfe, 0xff], true).unwrap();
        assert_eq!(
            report.0[1],
            Some(PadReport {
                state: Report(0, 0, 7, -2, 0, 0, 0),
                ds4: None,
            })
        );
        for invalid in [
            &[10, 2][..],
            &[10, 2, 0x80],
            &[10, 2, 1, 0],
            &[10, 2, 0x40, 0],
            &[10, 2, 0, 0],
        ] {
            assert!(
                Snapshot::parse_binary(invalid, true).is_err(),
                "{invalid:?}"
            );
        }
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
        fn neutralize_idles_pads_but_keeps_them_attached() {
            let updates = Arc::new(Mutex::new(Vec::new()));
            let mut gamepads = Gamepads::default();
            gamepads.pads[0] = Some(Pad {
                kind: PadKind::DualShock4,
                target: Box::new(ScriptedPad {
                    outcomes: VecDeque::new(),
                    updates: Arc::clone(&updates),
                }),
                last: None,
            });
            gamepads.last_snapshot = Some(Instant::now());

            gamepads.neutralize();
            assert!(gamepads.pads[0].is_some());
            assert_eq!(gamepads.last_snapshot, None);
            assert_eq!(
                updates.lock().unwrap().as_slice(),
                &[PadReport::neutral(PadKind::DualShock4)]
            );
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
