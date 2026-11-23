use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::{xkb, xproto, xtest};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const MAPPING_SETTLE: Duration = Duration::from_millis(75);
const CORE_KEYBOARD: u16 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stroke {
    keycode: u8,
    modifiers: u16,
}

pub(super) fn inject(text: &str) -> Result<()> {
    ensure!(!text.contains('\0'), "text contains a null byte");
    if text.is_empty() {
        return Ok(());
    }
    let (connection, screen) = x11rb::connect(None)?;
    ensure!(
        xkb::use_extension(&connection, 1, 0)?.reply()?.supported,
        "XKB is unavailable"
    );
    let state = xkb::get_state(&connection, CORE_KEYBOARD)?.reply()?;
    ensure!(
        u16::from(state.latched_mods) == 0,
        "release latched modifier keys before committing text"
    );
    let setup = connection.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode - first + 1;
    let original = connection.get_keyboard_mapping(first, count)?.reply()?;
    let width = original.keysyms_per_keycode;
    ensure!(width != 0, "empty X11 keyboard mapping");
    let map = xkb::get_map(
        &connection,
        CORE_KEYBOARD,
        xkb::MapPart::KEY_TYPES | xkb::MapPart::KEY_SYMS,
        0u16.into(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0u16.into(),
        0,
        0,
        0,
        0,
        0,
        0,
    )?
    .reply()?;
    let syms = map.map.syms_rtrn.context("missing XKB symbols")?;
    let types = map.map.types_rtrn.context("missing XKB types")?;
    let modifier_map = connection.get_modifier_mapping()?.reply()?;
    let pressed = connection.query_keymap()?.reply()?.keys;
    let held_modifiers: HashSet<u8> = modifier_map
        .keycodes
        .iter()
        .copied()
        .filter(|&code| code != 0 && pressed[code as usize / 8] & (1 << (code % 8)) != 0)
        .collect();
    let mut modifiers = Vec::new();
    for (index, codes) in modifier_map
        .keycodes
        .chunks(modifier_map.keycodes_per_modifier() as usize)
        .enumerate()
    {
        if let Some(&code) = codes.iter().find(|&&code| {
            code >= map.first_key_sym
                && syms
                    .get((code - map.first_key_sym) as usize)
                    .is_some_and(|key| {
                        key.syms
                            .iter()
                            .any(|sym| matches!(*sym, 0xffe1 | 0xffe2 | 0xfe03 | 0xfe11 | 0xff7e))
                    })
        }) {
            modifiers.push((1u16 << index, code));
        }
    }
    let mut layout = HashMap::new();
    for combination in 0..(1usize << modifiers.len()) {
        let mask = modifiers
            .iter()
            .enumerate()
            .filter(|(index, _)| combination & (1 << index) != 0)
            .fold(0, |mask, (_, (bit, _))| mask | bit);
        for (offset, key) in syms.iter().enumerate() {
            if let Some(symbol) = lookup(
                key,
                &types,
                u8::from(state.group),
                mask | u16::from(state.locked_mods),
            ) {
                layout.entry(symbol).or_insert(Stroke {
                    keycode: map.first_key_sym + offset as u8,
                    modifiers: mask,
                });
            }
        }
    }
    let spares = spare_keycodes(first, width, &original.keysyms);
    let spare = spares
        .first()
        .zip(spares.last())
        .map(|(&start, &end)| (start, end - start + 1));
    let symbols: Vec<u32> = text.chars().map(symbol).collect();
    let root = setup.roots[screen].root;
    let mut offset = 0;
    let mut changed = false;
    let mut synthetic = HashSet::new();
    let result = (|| -> Result<()> {
        for &code in &held_modifiers {
            event(&connection, root, code, false)?;
        }
        while offset < symbols.len() {
            let capacity = spares.len();
            let (end, unmapped) = chunk(&symbols, offset, &layout, capacity)?;
            let mut batch = layout.clone();
            if !unmapped.is_empty() {
                let (start, count) = spare.context("no spare X11 keycodes for Unicode text")?;
                let begin = (start - map.first_key_sym) as usize;
                // Preserve occupied rows between noncontiguous spare keycodes.
                let mut mapping = syms[begin..begin + count as usize].to_vec();
                for (index, sym) in unmapped.iter().enumerate() {
                    let keycode = spares[index];
                    mapping[(keycode - start) as usize] = xkb::KeySymMap {
                        kt_index: [0; 4],
                        group_info: 1,
                        width: 1,
                        syms: vec![*sym],
                    };
                    batch.insert(
                        *sym,
                        Stroke {
                            keycode,
                            modifiers: 0,
                        },
                    );
                }
                changed = true;
                set_mapping(&connection, start, mapping)?;
                thread::sleep(MAPPING_SETTLE);
            }
            for sym in &symbols[offset..end] {
                let stroke = batch[sym];
                for &(bit, code) in &modifiers {
                    if stroke.modifiers & bit != 0 {
                        synthetic.insert(code);
                        event(&connection, root, code, true)?;
                    }
                }
                synthetic.insert(stroke.keycode);
                event(&connection, root, stroke.keycode, true)?;
                event(&connection, root, stroke.keycode, false)?;
                synthetic.remove(&stroke.keycode);
                for &(bit, code) in modifiers.iter().rev() {
                    if stroke.modifiers & bit != 0 {
                        event(&connection, root, code, false)?;
                        synthetic.remove(&code);
                    }
                }
            }
            connection.sync()?;
            if changed {
                thread::sleep(MAPPING_SETTLE);
            }
            offset = end;
        }
        Ok(())
    })();
    let modifiers_restored = (|| -> Result<()> {
        if synthetic.is_empty() && held_modifiers.is_empty() {
            return Ok(());
        }
        for code in synthetic {
            event(&connection, root, code, false)?;
        }
        for code in held_modifiers {
            event(&connection, root, code, true)?;
        }
        connection.sync()?;
        Ok(())
    })();
    let restored = if changed {
        let (start, count) = spare.unwrap();
        let begin = (start - map.first_key_sym) as usize;
        set_mapping(
            &connection,
            start,
            syms[begin..begin + count as usize].to_vec(),
        )
    } else {
        Ok(())
    };
    result.and(modifiers_restored).and(restored)
}

fn set_mapping(connection: &RustConnection, first: u8, syms: Vec<xkb::KeySymMap>) -> Result<()> {
    let count = syms.len() as u8;
    let total = syms.iter().map(|key| key.syms.len() as u16).sum();
    let values = xkb::SetMapAux::new().syms(syms);
    // Core ChangeKeyboardMapping cannot round-trip XKB's multi-level maps.
    xkb::set_map(
        connection,
        CORE_KEYBOARD,
        0u16.into(),
        connection.setup().min_keycode,
        connection.setup().max_keycode,
        0,
        0,
        first,
        count,
        total,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0u16.into(),
        &values,
    )?
    .check()?;
    Ok(())
}

fn event(connection: &RustConnection, root: u32, keycode: u8, press: bool) -> Result<()> {
    xtest::fake_input(
        connection,
        if press {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        },
        keycode,
        0,
        root,
        0,
        0,
        0,
    )?;
    Ok(())
}

fn symbol(character: char) -> u32 {
    match character {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        _ => xkeysym::Keysym::from_char(character).raw(),
    }
}

fn lookup(key: &xkb::KeySymMap, types: &[xkb::KeyType], group: u8, modifiers: u16) -> Option<u32> {
    let groups = key.group_info & 0x0f;
    if groups == 0 {
        return None;
    }
    let group = if group < groups {
        group
    } else {
        match key.group_info & 0xc0 {
            0x40 => groups - 1,
            0x80 => ((key.group_info & 0x30) >> 4).min(groups - 1),
            _ => group % groups,
        }
    };
    let kind = types.get(key.kt_index[group as usize] as usize)?;
    let masked = modifiers & u16::from(kind.mods_mask);
    let level = kind
        .map
        .iter()
        .find(|entry| entry.active && u16::from(entry.mods_mask) == masked)
        .map_or(0, |entry| entry.level);
    key.syms
        .get(group as usize * key.width as usize + level as usize)
        .copied()
        .filter(|sym| *sym != 0)
}

fn spare_keycodes(first: u8, width: u8, mapping: &[u32]) -> Vec<u8> {
    mapping
        .chunks(width as usize)
        .enumerate()
        .filter_map(|(index, syms)| {
            let code = first + index as u8;
            (code != 8 && syms.iter().all(|sym| *sym == 0)).then_some(code)
        })
        .collect()
}

fn chunk(
    symbols: &[u32],
    start: usize,
    layout: &HashMap<u32, Stroke>,
    capacity: usize,
) -> Result<(usize, Vec<u32>)> {
    let mut unmapped = Vec::new();
    let mut end = start;
    for &symbol in &symbols[start..] {
        if !layout.contains_key(&symbol) && !unmapped.contains(&symbol) {
            if unmapped.len() == capacity {
                break;
            }
            unmapped.push(symbol);
        }
        end += 1;
    }
    ensure!(
        end > start || start == symbols.len(),
        "no spare X11 keycodes for Unicode text"
    );
    Ok((end, unmapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_preserve_order_and_reuse_mappings() {
        let layout = HashMap::from([(
            1,
            Stroke {
                keycode: 9,
                modifiers: 0,
            },
        )]);
        assert_eq!(
            chunk(&[1, 2, 1, 2, 3], 0, &layout, 1).unwrap(),
            (4, vec![2])
        );
        assert_eq!(chunk(&[], 0, &layout, 0).unwrap(), (0, vec![]));
    }

    #[test]
    fn refuses_unmapped_text_without_spares() {
        assert!(chunk(&[2], 0, &HashMap::new(), 0).is_err());
        assert!(inject("bad\0text").is_err());
        inject("").unwrap();
    }

    #[test]
    fn spare_range_never_overwrites_populated_codes() {
        assert_eq!(
            spare_keycodes(8, 2, &[0, 0, 1, 0, 0, 0, 0, 0]),
            vec![10, 11]
        );
        assert!(spare_keycodes(9, 2, &[1, 0]).is_empty());
    }

    #[test]
    fn looks_up_shift_and_altgr_levels_without_remapping() {
        let types = vec![xkb::KeyType {
            mods_mask: 129u16.into(),
            num_levels: 4,
            map: [(1u16, 1), (128, 2), (129, 3)]
                .map(|(mods, level)| xkb::KTMapEntry {
                    active: true,
                    mods_mask: mods.into(),
                    level,
                    ..Default::default()
                })
                .to_vec(),
            ..Default::default()
        }];
        let key = xkb::KeySymMap {
            group_info: 1,
            width: 4,
            syms: vec![97, 65, 233, 201],
            ..Default::default()
        };
        for (mask, expected) in [(0, 97), (1, 65), (128, 233), (129, 201)] {
            assert_eq!(lookup(&key, &types, 0, mask), Some(expected));
        }
        assert_eq!(lookup(&xkb::KeySymMap::default(), &types, 0, 0), None);
    }
}
