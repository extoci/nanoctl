use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU32, Ordering},
};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("NANOCTL_BENCH_TRACE").as_deref() == Ok("1"))
}

pub fn now() -> f64 {
    if !enabled() {
        return 0.0;
    }
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // The writable timespec remains valid for this synchronous call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0.0;
    }
    time.tv_sec as f64 * 1000.0 + time.tv_nsec as f64 / 1_000_000.0
}

pub fn emit(stage: &str, sequence: u32, at: f64) {
    if enabled() && at > 0.0 {
        tracing::info!(target: "nanoctl::bench_trace", stage, sequence, mono_ms = at, "benchmark attribution");
    }
}

fn marker(width: usize, height: usize, pixels: &[u8]) -> Option<u16> {
    if width < 144 || width.checked_mul(height)?.checked_mul(4)? != pixels.len() {
        return None;
    }
    static ROW: Mutex<Option<usize>> = Mutex::new(None);
    let mut cached = ROW.lock().unwrap();
    if let Some(y) = *cached
        && y < height
        && let Some(sequence) = marker_row(&pixels[y * width * 4..(y + 1) * width * 4])
    {
        return Some(sequence);
    }
    for y in (0..height).step_by(4) {
        if let Some(sequence) = marker_row(&pixels[y * width * 4..(y + 1) * width * 4]) {
            *cached = Some(y);
            return Some(sequence);
        }
    }
    None
}

fn marker_row(row: &[u8]) -> Option<u16> {
    let mut left = None;
    for (x, pixel) in row.chunks_exact(4).enumerate() {
        if pixel[0] < 80 && pixel[1] > 150 && pixel[2] > 150 {
            left = Some(x);
        }
        if let Some(left) = left
            && pixel[0] > 150
            && pixel[1] < 80
            && pixel[2] > 150
        {
            if x - left < 144 {
                return None;
            }
            let mut bits = [0_u8; 36];
            for (i, bit) in bits.iter_mut().enumerate() {
                let red = row[(left + (x - left) * (2 * i + 1) / 72) * 4];
                *bit = if red > 192 {
                    1
                } else if red < 64 {
                    0
                } else {
                    return None;
                };
            }
            if bits[..2] != [1, 0] || bits[34..] != [0, 1] {
                return None;
            }
            let word = bits[2..34]
                .iter()
                .fold(0_u32, |word, bit| (word << 1) | u32::from(*bit));
            let sequence = word >> 16;
            let input = (word >> 8) & 255;
            return ((word & 255) == ((sequence >> 8) ^ sequence ^ input ^ 0xa5) & 255)
                .then_some(sequence as u16);
        }
    }
    None
}

pub fn capture(frame: &xcap::image::RgbaImage, damage: f64, start: f64, end: f64) {
    if enabled()
        && let Some(sequence) = marker(
            frame.width() as usize,
            frame.height() as usize,
            frame.as_raw(),
        )
    {
        for (stage, at) in [
            ("damage", damage),
            ("capture_start", start),
            ("capture_end", end),
        ] {
            emit(stage, u32::from(sequence), at);
        }
    }
}

fn encoded_frames() -> &'static Mutex<HashMap<u64, u16>> {
    static FRAMES: OnceLock<Mutex<HashMap<u64, u16>>> = OnceLock::new();
    FRAMES.get_or_init(Mutex::default)
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}

pub fn encoded(frame: &xcap::Frame, bytes: &[u8], start: f64, end: f64) {
    if !enabled() || bytes.is_empty() {
        return;
    }
    if let Some(sequence) = marker(frame.width as usize, frame.height as usize, &frame.raw) {
        let mut frames = encoded_frames().lock().unwrap();
        if frames.len() >= 8192 {
            frames.clear();
        }
        frames.insert(fingerprint(bytes), sequence);
        emit("encode_start", u32::from(sequence), start);
        emit("encode_end", u32::from(sequence), end);
    }
}

pub fn sent(bytes: &[u8], start: f64, end: f64) {
    if !enabled() {
        return;
    }
    if let Some(sequence) = encoded_frames().lock().unwrap().remove(&fingerprint(bytes)) {
        emit("send_start", u32::from(sequence), start);
        emit("send_end", u32::from(sequence), end);
    }
}

pub fn input(stage: usize, bytes: &[u8]) {
    if !enabled() {
        return;
    }
    let at = now();
    let Ok(message) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    if message["type"] != "key" || message["action"] != "down" || message["code"] != "Space" {
        return;
    }
    static COUNTS: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
    let sequence = COUNTS[stage].fetch_add(1, Ordering::Relaxed) + 1;
    emit(
        ["input_receive", "inject_start", "inject_end"][stage],
        sequence,
        at,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_rejects_missing_corrupt_or_invalid_pixels() {
        assert_eq!(marker(0, 0, &[]), None);
        assert_eq!(marker(1280, 720, &[]), None);
        let mut pixels = vec![0; 1280 * 720 * 4];
        assert_eq!(marker(1280, 720, &pixels), None);
        let sequence = 1234_u32;
        let input = 17;
        let word =
            (sequence << 16) | (input << 8) | (((sequence >> 8) ^ sequence ^ input ^ 0xa5) & 255);
        let bits: Vec<_> = [1, 0]
            .into_iter()
            .chain((0..32).rev().map(|i| ((word >> i) & 1) as u8))
            .chain([0, 1])
            .collect();
        for x in 0..880 {
            let color = if x < 80 {
                [0, 255, 255, 255]
            } else if x >= 800 {
                [255, 0, 255, 255]
            } else {
                [bits[(x - 80) / 20] * 255; 4]
            };
            pixels[(120 * 1280 + x) * 4..(120 * 1280 + x + 1) * 4].copy_from_slice(&color);
        }
        assert_eq!(marker(1280, 720, &pixels), Some(1234));
        pixels[(120 * 1280 + 790) * 4] = 0;
        assert_eq!(marker(1280, 720, &pixels), None);
    }
}
