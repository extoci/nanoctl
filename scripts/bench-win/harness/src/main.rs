// nanoctl Windows benchmark harness: source marker band + high-rate pixel detector + input.
// Same 36-cell marker scheme as tests/browser/latency-marker.ts:
//   cyan guard | 36 B/W cells | magenta guard ; bits [1,0, word32, 0,1]
//   word = seq<<16 | input<<8 | checksum; checksum = (seq>>8)^seq^input^0xa5 & 0xff
//
//   benchwin band [--seconds N] [--fps N] [--height N] [--motion]
//   benchwin watch <client-title> <out.jsonl> [--seconds N] [--band-title S]
//   benchwin click <x> <y>
//   benchwin click-client <client-title>
//   benchwin windows
//   benchwin locate <title>
//   benchwin move <title> <x> <y> <w> <h>

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------- marker codec ----------

fn marker_bits(seq: u16, input: u8) -> [u8; 36] {
    let checksum = (((seq >> 8) ^ seq) as u8 ^ input ^ 0xa5) & 0xff;
    let word = ((seq as u32) << 16) | ((input as u32) << 8) | checksum as u32;
    let mut bits = [0u8; 36];
    bits[0] = 1;
    for i in 0..32 {
        bits[2 + i] = ((word >> (31 - i)) & 1) as u8;
    }
    bits[35] = 1;
    bits
}

/// Decode one BGRA row. Returns (sequence, input).
fn read_marker_row(px: &[u8], width: usize) -> Option<(u16, u8)> {
    let mut left = -1i32;
    let mut right = -1i32;
    for x in 0..width {
        let (b, g, r) = (px[x * 4], px[x * 4 + 1], px[x * 4 + 2]);
        if r < 80 && g > 150 && b > 150 {
            left = x as i32;
        }
        if left >= 0 && r > 150 && g < 80 && b > 150 {
            right = x as i32;
            break;
        }
    }
    if left < 0 || right - left < 96 {
        return None;
    }
    let mut bits = [-1i32; 36];
    for i in 0..36 {
        let x = (left + (right - left) * (2 * i as i32 + 1) / 72) as usize;
        if x >= width {
            return None;
        }
        let red = px[x * 4 + 2];
        bits[i] = if red > 192 {
            1
        } else if red < 64 {
            0
        } else {
            -1
        };
    }
    if bits[0] != 1 || bits[1] != 0 || bits[34] != 0 || bits[35] != 1 {
        return None;
    }
    let mut word: u32 = 0;
    for &b in &bits[2..34] {
        if b < 0 {
            return None;
        }
        word = (word << 1) | b as u32;
    }
    let seq = (word >> 16) as u16;
    let input = ((word >> 8) & 0xff) as u8;
    if (word & 0xff) as u8 != (((seq >> 8) ^ seq) as u8 ^ input ^ 0xa5) & 0xff {
        return None;
    }
    Some((seq, input))
}

fn find_marker_row(px: &[u8], width: usize, height: usize, stride: usize) -> Option<usize> {
    let mut row = vec![0u8; width * 4];
    let mut y = 0;
    while y < height {
        row.copy_from_slice(&px[y * stride..y * stride + width * 4]);
        if read_marker_row(&row, width).is_some() {
            return Some(y);
        }
        y += 2;
    }
    None
}

// ---------- screen strip grabber ----------

struct Strip {
    rect: RECT,
    width: usize,
    height: usize,
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pixels: Vec<u8>,
    stride: usize,
}

impl Strip {
    fn new(rect: RECT) -> Strip {
        let (width, height) = (
            (rect.right - rect.left).max(1) as usize,
            (rect.bottom - rect.top).max(1) as usize,
        );
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, width as i32, height as i32);
            let old = SelectObject(mem, bmp.into());
            Strip {
                rect,
                width,
                height,
                screen,
                mem,
                bmp,
                old,
                pixels: vec![0; width * 4 * height],
                stride: width * 4,
            }
        }
    }

    fn grab(&mut self) {
        unsafe {
            let _ = BitBlt(
                self.mem,
                0,
                0,
                self.width as i32,
                self.height as i32,
                Some(self.screen),
                self.rect.left,
                self.rect.top,
                SRCCOPY,
            );
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: self.width as i32,
                    biHeight: -(self.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    ..Default::default()
                },
                ..Default::default()
            };
            GetDIBits(
                self.mem,
                self.bmp,
                0,
                self.height as u32,
                Some(self.pixels.as_mut_ptr() as *mut c_void),
                &mut info,
                DIB_RGB_COLORS,
            );
        }
    }

    fn decode_marker(&self) -> Option<(u16, u8)> {
        let mut row = vec![0u8; self.stride];
        let y0 = self.height / 2;
        for y in y0.saturating_sub(2)..=(y0 + 2).min(self.height - 1) {
            row.copy_from_slice(&self.pixels[y * self.stride..(y + 1) * self.stride]);
            if let Some(m) = read_marker_row(&row, self.width) {
                return Some(m);
            }
        }
        None
    }
}

impl Drop for Strip {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.mem);
            ReleaseDC(None, self.screen);
        }
    }
}

// ---------- window helpers ----------

thread_local! {
    static FOUND: RefCell<Vec<(HWND, String)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn enum_cb(hwnd: HWND, _lp: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() {
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n > 0 {
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            FOUND.with(|f| f.borrow_mut().push((hwnd, title)));
        }
    }
    BOOL(1)
}

fn find_window(substr: &str) -> Option<HWND> {
    FOUND.with(|f| f.borrow_mut().clear());
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(0));
    }
    FOUND.with(|f| {
        f.borrow()
            .iter()
            .find(|(_, t)| t.to_lowercase().contains(&substr.to_lowercase()))
            .map(|(h, _)| *h)
    })
}

fn list_windows() {
    FOUND.with(|f| f.borrow_mut().clear());
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(0));
    }
    FOUND.with(|f| {
        for (h, t) in f.borrow().iter() {
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(*h, &mut r);
            }
            println!(
                "{} [{},{} {}x{}] {}",
                h.0 as usize,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                t
            );
        }
    });
}

fn client_rect(hwnd: HWND) -> RECT {
    unsafe {
        let mut r = RECT::default();
        let _ = GetClientRect(hwnd, &mut r);
        let mut p = POINT {
            x: r.left,
            y: r.top,
        };
        let _ = ClientToScreen(hwnd, &mut p);
        RECT {
            left: p.x,
            top: p.y,
            right: p.x + (r.right - r.left),
            bottom: p.y + (r.bottom - r.top),
        }
    }
}

// ---------- band window ----------

static mut SEQ: u16 = 0;
static mut INPUT_COUNT: u8 = 0;
static mut TICK: u32 = 0;
static mut MOTION: bool = false;
static mut NOISE: bool = false;
static mut FLASH: bool = false;
static mut CLOCK_MODE: bool = false;

fn clock_ms() -> u16 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        % 60_000) as u16
}

const CELLS: usize = 36;

unsafe fn band_paint_dc(hwnd: HWND, dc: HDC) {
    let mut r = RECT::default();
    let _ = GetClientRect(hwnd, &mut r);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let guard = 24i32;
    let top = 4i32;
    let mh = (h - 8).min(40);
    let bits = marker_bits(SEQ, INPUT_COUNT);
    let cyan = CreateSolidBrush(COLORREF(0x00FFFF00)); // COLORREF = 0x00BBGGRR → cyan
    let magenta = CreateSolidBrush(COLORREF(0x00FF00FF));
    let black = CreateSolidBrush(COLORREF(0));
    let white = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let mut cell = RECT {
        left: 0,
        top,
        right: guard,
        bottom: top + mh,
    };
    FillRect(dc, &cell, cyan);
    let span = w - 2 * guard;
    for i in 0..CELLS {
        cell.left = guard + span * i as i32 / CELLS as i32;
        cell.right = guard + span * (i as i32 + 1) / CELLS as i32;
        FillRect(dc, &cell, if bits[i] == 1 { white } else { black });
    }
    cell.left = w - guard;
    cell.right = w;
    FillRect(dc, &cell, magenta);
    if FLASH && h > 56 {
        // Whole-zone black↔white flash per tick — impossible for the encoder
        // to motion-compensate; isolates "capture didn't see motion" from
        // "encoder compressed motion to nothing".
        let y0 = top + mh + 4;
        let zone = RECT {
            left: 0,
            top: y0,
            right: w,
            bottom: h,
        };
        FillRect(dc, &zone, if TICK & 1 == 0 { black } else { white });
    } else if NOISE && h > 56 {
        // Per-pixel random noise, alternating two pre-generated DIBs each
        // tick. Flat-color cells compress to ~1KB/AU (flat MBs); pixel-level
        // noise cannot be motion-compensated or intra-predicted — the honest
        // worst case for encode size and encode time.
        let y0 = top + mh + 4;
        let zone_w = w;
        let zone_h = h - y0;
        static mut NOISE_BITS: Option<Vec<Vec<u8>>> = None;
        static mut NOISE_WH: (i32, i32) = (0, 0);
        if NOISE_BITS.is_none() || NOISE_WH != (zone_w, zone_h) {
            let mut fields = Vec::with_capacity(2);
            for t in 0..2u64 {
                let mut seed = 0x9E3779B97F4A7C15u64 ^ (t + 1);
                let mut px = vec![0u8; (zone_w * zone_h * 3).max(0) as usize];
                for chunk in px.chunks_mut(8) {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    chunk.copy_from_slice(&seed.to_le_bytes()[..chunk.len()]);
                }
                fields.push(px);
            }
            NOISE_BITS = Some(fields);
            NOISE_WH = (zone_w, zone_h);
        }
        let fields = NOISE_BITS.as_ref().unwrap();
        let px = &fields[(TICK & 1) as usize];
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: zone_w,
                biHeight: -zone_h, // top-down
                biPlanes: 1,
                biBitCount: 24,
                biCompression: 0, // BI_RGB
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            ..Default::default()
        };
        let _ = StretchDIBits(
            dc,
            0,
            y0,
            zone_w,
            zone_h,
            0,
            0,
            zone_w,
            zone_h,
            Some(px.as_ptr() as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    } else if MOTION && h > 56 {
        // Gaming-like motion: 16px checkerboard scrolled right 4px per paint
        // tick — uniform translation that motion estimation should predict,
        // so P-frames stay cheap only if ME actually works. The pattern is
        // pre-rendered to a memory bitmap once and shifted with a single
        // BitBlt per frame: thousands of per-tile FillRects made the painter
        // itself the bottleneck (~17fps).
        let y0 = top + mh + 4;
        static mut TILE_DC: Option<HDC> = None;
        static mut TILE_BMP: Option<HBITMAP> = None;
        static mut TILE_WH: (i32, i32) = (0, 0);
        let span = w + 64;
        if TILE_BMP.is_none() || TILE_WH != (span, h - y0 + 64) {
            let mem = CreateCompatibleDC(Some(dc));
            let bmp = CreateCompatibleBitmap(dc, span, h - y0 + 64);
            // Select the bitmap into the memory DC *before* filling it —
            // otherwise the checkerboard lands on the DC's default 1x1
            // bitmap and the blit below copies a blank surface.
            let _stock = SelectObject(mem, bmp.into());
            let black = CreateSolidBrush(COLORREF(0));
            let white = CreateSolidBrush(COLORREF(0x00FFFFFF));
            let mut yy = 0i32;
            while yy < h - y0 + 64 {
                let mut xx = 0i32;
                while xx < span {
                    let rc = RECT {
                        left: xx,
                        top: yy,
                        right: xx + 16,
                        bottom: yy + 16,
                    };
                    FillRect(
                        mem,
                        &rc,
                        if (xx / 16 + yy / 16) & 1 == 0 {
                            black
                        } else {
                            white
                        },
                    );
                    xx += 16;
                }
                yy += 16;
            }
            let _ = DeleteObject(black.into());
            let _ = DeleteObject(white.into());
            if let Some(old) = TILE_BMP.replace(bmp) {
                let _ = DeleteObject(old.into());
            }
            TILE_DC = Some(mem);
            TILE_WH = (span, h - y0 + 64);
        }
        let mem = TILE_DC.unwrap();
        let prev = SelectObject(mem, TILE_BMP.unwrap().into());
        let scroll = ((TICK as i32 % 256) * 4) % 32;
        // Wrap scroll into one checker period (32px): alternate the two phase
        // offsets so the pattern translates smoothly.
        let phase_a = scroll % 32;
        let _ = BitBlt(
            dc,
            -phase_a - 16,
            y0,
            span,
            h - y0 + 64,
            Some(mem),
            0,
            0,
            SRCCOPY,
        );
        SelectObject(mem, prev);
    }
    let _ = DeleteObject(cyan.into());
    let _ = DeleteObject(magenta.into());
    let _ = DeleteObject(black.into());
    let _ = DeleteObject(white.into());
}

unsafe extern "system" fn band_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            INPUT_COUNT = INPUT_COUNT.wrapping_add(1);
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            band_paint_dc(hwnd, dc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn cmd_band(args: &[String]) -> i32 {
    let fps = arg_int(args, "--fps", 60).clamp(1, 240) as u32;
    let height = arg_int(args, "--height", 56);
    let toolwindow = !args.iter().any(|a| a == "--no-toolwindow");
    unsafe {
        MOTION = args.iter().any(|a| a == "--motion");
        NOISE = args.iter().any(|a| a == "--noise");
        FLASH = args.iter().any(|a| a == "--flash");
        CLOCK_MODE = args.iter().any(|a| a == "--clock");
    }
    let seconds = arg_int(args, "--seconds", 0);
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpszClassName: w!("nanoctl-bench-band"),
            lpfnWndProc: Some(band_proc),
            hInstance: instance.into(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let w = GetSystemMetrics(SM_CXSCREEN);
        let ex = if toolwindow {
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
        } else {
            WS_EX_TOPMOST | WS_EX_NOACTIVATE
        };
        let hwnd = CreateWindowExW(
            ex,
            w!("nanoctl-bench-band"),
            w!("nanoctl-bench-band"),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            w,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .expect("band window");
        timeBeginPeriod(1);
        // Dedicated painter: redraws the marker directly to the window DC at `fps`,
        // so the on-screen marker advances at the display composite rate rather
        // than at WM_TIMER/WM_PAINT coalescing granularity.
        let paint_hz = (fps.max(2) as f64 / 2.0) as u32;
        let hwnd_raw = hwnd.0 as usize;
        std::thread::spawn(move || {
            let hwnd = HWND(hwnd_raw as *mut _);
            unsafe {
                let _ = timeBeginPeriod(1);
                loop {
                    if IsWindow(Some(hwnd)).as_bool() == false {
                        break;
                    }
                    if CLOCK_MODE {
                        SEQ = clock_ms();
                    } else {
                        SEQ = SEQ.wrapping_add(1);
                    }
                    TICK = TICK.wrapping_add(1);
                    let dc = GetDC(Some(hwnd));
                    if !dc.is_invalid() {
                        band_paint_dc(hwnd, dc);
                        let _ = ReleaseDC(Some(hwnd), dc);
                    }
                    std::thread::sleep(Duration::from_millis((1000 / paint_hz) as u64));
                }
            }
        });
        SetTimer(Some(hwnd), 1, (1000 / fps).max(1), None);
        if seconds > 0 {
            SetTimer(Some(hwnd), 2, (seconds * 1000) as u32, None);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            if msg.message == WM_TIMER && msg.wParam.0 == 2 {
                let _ = DestroyWindow(hwnd);
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

// ---------- input ----------

fn send_click(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
        std::thread::sleep(Duration::from_millis(15));
        let mk = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [mk(MOUSEEVENTF_LEFTDOWN), mk(MOUSEEVENTF_LEFTUP)];
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn now_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

// ---------- watch ----------

fn cmd_watch(args: &[String]) -> i32 {
    let client_title = &args[1];
    let out_path = &args[2];
    let seconds = arg_int(args, "--seconds", 60) as u64;
    let band_title = arg_str(args, "--band-title", "nanoctl-bench-band");

    let band = find_window(&band_title);
    let client = find_window(client_title);
    if band.is_none() {
        eprintln!("band window not found");
        return 2;
    }
    if client.is_none() {
        eprintln!("client window not found: {client_title}");
        return 2;
    }
    let band_rect = client_rect(band.unwrap());
    let client_rect = client_rect(client.unwrap());
    eprintln!(
        "band {}x{}@{},{} client {}x{}@{},{}",
        band_rect.right - band_rect.left,
        band_rect.bottom - band_rect.top,
        band_rect.left,
        band_rect.top,
        client_rect.right - client_rect.left,
        client_rect.bottom - client_rect.top,
        client_rect.left,
        client_rect.top
    );

    // locate marker row inside the client window (up to 30s)
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client_row: Option<usize> = None;
    while client_row.is_none() && Instant::now() < deadline {
        let mut scan = Strip::new(client_rect);
        scan.grab();
        let w = (client_rect.right - client_rect.left) as usize;
        let h = (client_rect.bottom - client_rect.top) as usize;
        client_row = find_marker_row(&scan.pixels, w, h, scan.stride);
        if client_row.is_none() {
            drop(scan);
            std::thread::sleep(Duration::from_millis(400));
        }
    }
    let Some(row) = client_row else {
        eprintln!("marker not visible in client window");
        return 3;
    };
    eprintln!("client marker row at client-relative y={row}");

    let band_mid = (band_rect.bottom - band_rect.top) / 2;
    let mut band_strip = Strip::new(RECT {
        left: band_rect.left,
        top: band_rect.top + band_mid - 5,
        right: band_rect.right,
        bottom: band_rect.top + band_mid + 5,
    });
    let mut client_strip = Strip::new(RECT {
        left: client_rect.left,
        top: client_rect.top + row as i32 - 3,
        right: client_rect.right,
        bottom: client_rect.top + row as i32 + 5,
    });

    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path).unwrap());
    let mut band_vals: Vec<(f64, (u16, u8))> = Vec::new();
    let mut client_vals: Vec<(f64, (u16, u8))> = Vec::new();
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    let mut last_band: Option<(u16, u8)> = None;
    let mut last_client: Option<(u16, u8)> = None;
    while Instant::now() < end {
        let next = Instant::now() + Duration::from_micros(4167);
        band_strip.grab();
        client_strip.grab();
        let t = now_ms(start);
        if let Some(m) = band_strip.decode_marker() {
            band_vals.push((t, m));
            if last_band != Some(m) {
                let _ = writeln!(
                    out,
                    "{{\"t\":{t:.3},\"src\":\"band\",\"seq\":{},\"input\":{}}}",
                    m.0, m.1
                );
                last_band = Some(m);
            }
        }
        if let Some(m) = client_strip.decode_marker() {
            client_vals.push((t, m));
            if last_client != Some(m) {
                let _ = writeln!(
                    out,
                    "{{\"t\":{t:.3},\"src\":\"client\",\"seq\":{},\"input\":{}}}",
                    m.0, m.1
                );
                last_client = Some(m);
            }
        }
        let now = Instant::now();
        if next > now {
            let d = next - now;
            if d > Duration::from_millis(3) {
                std::thread::sleep(d - Duration::from_millis(2));
            }
            std::hint::spin_loop();
        }
    }
    let mut bi = 0;
    for (ct, cm) in &client_vals {
        while bi + 1 < band_vals.len() && band_vals[bi + 1].0 <= *ct {
            bi += 1;
        }
        if let Some((bt, bm)) = band_vals.get(bi) {
            let _ =
                writeln!(
                out,
                "{{\"t\":{ct:.3},\"src\":\"pair\",\"bandSeq\":{},\"clientSeq\":{},\"dt\":{:.3}}}",
                bm.0, cm.0, ct - bt
            );
        }
    }
    let _ = writeln!(
        out,
        "{{\"t\":{:.3},\"src\":\"done\",\"clientRow\":{},\"band\":[{},{},{},{}],\"client\":[{},{},{},{}]}}",
        now_ms(start),
        row,
        band_rect.left,
        band_rect.top,
        band_rect.right - band_rect.left,
        band_rect.bottom - band_rect.top,
        client_rect.left,
        client_rect.top,
        client_rect.right - client_rect.left,
        client_rect.bottom - client_rect.top
    );
    out.flush().unwrap();
    0
}

// ---------- args ----------

fn arg_int(args: &[String], flag: &str, default: i32) -> i32 {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn arg_str(args: &[String], flag: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("band|watch|click|click-client|windows|locate|move");
        std::process::exit(2);
    }
    let code = match args[0].as_str() {
        "band" => cmd_band(&args),
        "watch" => cmd_watch(&args),
        "click" => {
            send_click(args[1].parse().unwrap(), args[2].parse().unwrap());
            0
        }
        "click-client" => {
            let Some(hwnd) = find_window(&args[1]) else {
                eprintln!("window not found");
                std::process::exit(2);
            };
            let rect = client_rect(hwnd);
            let mut scan = Strip::new(rect);
            scan.grab();
            let w = (rect.right - rect.left) as usize;
            let h = (rect.bottom - rect.top) as usize;
            match find_marker_row(&scan.pixels, w, h, scan.stride) {
                Some(row) => {
                    let mid = (row as i32).min(h as i32 - 1);
                    send_click(rect.left + (rect.right - rect.left) * 2 / 5, rect.top + mid);
                    0
                }
                None => {
                    eprintln!("marker not found in client");
                    3
                }
            }
        }
        "wgc-frame" => match cmd_wgc_frame(
            args.get(1)
                .map(|s| s.as_str())
                .unwrap_or("C:/Users/Administrator/bench/wgc.png"),
        ) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("wgc-frame: {}", e);
                1
            }
        },
        "windows" => {
            list_windows();
            0
        }
        "dump" => {
            // dump <x> <y> <w> <h> — print per-row decode result + first pixels
            let rect = RECT {
                left: args[1].parse().unwrap(),
                top: args[2].parse().unwrap(),
                right: args[1].parse::<i32>().unwrap() + args[3].parse::<i32>().unwrap(),
                bottom: args[2].parse::<i32>().unwrap() + args[4].parse::<i32>().unwrap(),
            };
            let mut scan = Strip::new(rect);
            scan.grab();
            let w = (rect.right - rect.left) as usize;
            let h = (rect.bottom - rect.top) as usize;
            eprintln!(
                "grab {}x{} first bytes {:02x?}",
                w,
                h,
                &scan.pixels[..16.min(scan.pixels.len())]
            );
            for y in 0..h {
                let row = &scan.pixels[y * scan.stride..(y + 1) * scan.stride];
                if let Some(m) = read_marker_row(row, w) {
                    println!("y={y} seq={} input={}", m.0, m.1);
                }
            }
            0
        }
        "locate" => {
            let Some(hwnd) = find_window(&args[1]) else {
                eprintln!("window not found");
                std::process::exit(2);
            };
            let rect = client_rect(hwnd);
            let mut scan = Strip::new(rect);
            scan.grab();
            let w = (rect.right - rect.left) as usize;
            let h = (rect.bottom - rect.top) as usize;
            let row = find_marker_row(&scan.pixels, w, h, scan.stride);
            println!(
                "{},{} {}x{} marker_y={:?}",
                rect.left,
                rect.top,
                w,
                h,
                row.map(|v| v as i64).unwrap_or(-1)
            );
            if row.is_none() {
                3
            } else {
                0
            }
        }
        "move" => {
            let Some(hwnd) = find_window(&args[1]) else {
                eprintln!("window not found");
                std::process::exit(2);
            };
            let (x, y, w, h) = (
                args[2].parse().unwrap(),
                args[3].parse().unwrap(),
                args[4].parse().unwrap(),
                args[5].parse().unwrap(),
            );
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            0
        }
        "grab" => {
            let path = args.get(1).map(String::as_str).unwrap_or("grab.png");
            match cmd_wgc_frame(path) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        "recgrab" => {
            // Pull one frame through the streaming recorder (the same path
            // media-smoke uses) so capture-pipeline staleness is visible.
            let n = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(5);
            match cmd_wgc_recorder(n) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        other => {
            eprintln!("unknown: {other}");
            2
        }
    };
    std::process::exit(code);
}

fn cmd_wgc_recorder(n: u32) -> Result<(), String> {
    use xcap::Monitor;
    let monitors = Monitor::all().map_err(|e| e.to_string())?;
    let m = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .ok_or("no monitor")?;
    let (recorder, rx) = m.video_recorder().map_err(|e| e.to_string())?;
    recorder.start().map_err(|e| e.to_string())?;
    let mut hashes = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2000);
    while hashes.len() < n as usize && std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(frame) => {
                // Fingerprint only the checkerboard zone (below the 56px
                // marker row), so marker updates alone can't explain changes.
                let row_bytes = frame.width as usize * 4;
                let start = row_bytes * 56;
                let mut h: u64 = 1469598103934665603;
                for (i, b) in frame.raw.iter().enumerate().skip(start) {
                    if i % 97 == 0 {
                        h = (h ^ u64::from(*b)).wrapping_mul(1099511628211);
                    }
                }
                hashes.push(h);
                if hashes.len() == 4 {
                    if let Some(img) =
                        image::RgbaImage::from_raw(frame.width, frame.height, frame.raw.clone())
                    {
                        let _ = img.save("C:\\Users\\Administrator\\bench\\recframe.png");
                    }
                }
            }
            Err(_) => hashes.push(0),
        }
    }
    recorder.stop().map_err(|e| e.to_string())?;
    println!("{:?}", hashes);
    Ok(())
}

fn cmd_wgc_frame(path: &str) -> Result<(), String> {
    use xcap::Monitor;
    let monitors = Monitor::all().map_err(|e| e.to_string())?;
    let m = monitors
        .into_iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .ok_or("no monitor")?;
    let img = m.capture_image().map_err(|e| e.to_string())?;
    img.save(path).map_err(|e| e.to_string())?;
    println!("saved {}", path);
    Ok(())
}
