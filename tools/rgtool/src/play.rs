//! Running and driving the built Windows game: `run`, `lan-test`, `keys`, `shot`, `killgame`.
//! Logs go to build/logs, screenshots to build/shots.

use crate::util::{root, R};
use crate::windows::exe;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GAME_EXE: &str = "ResetGeneration-Revived.exe";

fn logs() -> PathBuf {
    let d = root().join("build/logs");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn spawn(args: &[String], log: &str) -> R<u32> {
    if !exe().exists() {
        return Err("build first: cargo x build-windows".into());
    }
    let f = std::fs::File::create(logs().join(log))?;
    crate::util::no_inherit();
    let child = Command::new(exe()).args(args).stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f).spawn()?;
    Ok(child.id())
}

/// Starts the game (arguments as on its command line, e.g. --nickname=Bob --server=lan).
pub fn run(args: &[String]) -> R<()> {
    let pid = spawn(args, "run.log")?;
    println!("started pid {pid}, log {}", logs().join("run.log").display());
    Ok(())
}

pub fn killgame() -> R<()> {
    for im in [GAME_EXE, "rg-relay.exe"] {
        let _ = Command::new("taskkill").args(["/IM", im, "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    Ok(())
}

/// Several games on this PC: one hosts a match for N players, the others join it with Quick
/// Match (menus driven by --keys). `--server=X` picks the relay (default auto).
pub fn lan_test(args: &[String]) -> R<()> {
    let mut n = 2;
    let mut server = "auto".to_string();
    for a in args {
        if let Some(s) = a.strip_prefix("--server=") {
            server = s.to_string();
        } else if let Ok(v) = a.parse::<usize>() {
            n = v.clamp(2, 4);
        }
    }
    killgame()?;
    let s = format!("--server={server}");
    let host_log = logs().join("host.log");
    spawn(
        &[
            "--nickname=Host".into(),
            s.clone(),
            "--host=auto".into(),
            format!("--host_players={n}"),
            "--keys=6000:ESC".into(),
            "--window_scale=1".into(),
            "--window_x=20".into(),
            "--window_y=20".into(),
        ],
        "host.log",
    )?;
    // The others start once the host is hosting: that also keeps them from patching the game data
    // while the host does (first launch per resolution).
    let t = Instant::now();
    while !std::fs::read_to_string(&host_log).unwrap_or_default().contains("HostServer: hosting") {
        if t.elapsed() > Duration::from_secs(90) {
            return Err(format!("the host did not start hosting, see {}", host_log.display()).into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // intro, main menu (ESC opens Quit -> No), down to Quick Match, pick the first hero
    let keys = "--keys=6000:ESC,8000:ESC,9500:RETURN,10500:DOWN,11000:DOWN,11500:RETURN,14500:RETURN";
    for i in 2..=n {
        spawn(
            &[
                format!("--nickname=Player{i}"),
                s.clone(),
                keys.into(),
                "--window_scale=1".into(),
                format!("--window_x={}", (i - 1) % 2 * 680 + 20),
                format!("--window_y={}", (i - 1) / 2 * 420 + 20),
            ],
            &format!("player{i}.log"),
        )?;
        std::thread::sleep(Duration::from_secs(2));
    }
    println!("started {n} games (logs: {}); the match begins about 15 s after the last one", logs().display());
    Ok(())
}

/// `keys "RETURN,DOWN,WAIT500" [shot-name] [--pid=N]`: key presses to the game window, then a
/// screenshot (build/shots/<name>.png).
pub fn keys(args: &[String]) -> R<()> {
    let (pid, rest) = pid_arg(args);
    let keys = rest.first().cloned().unwrap_or_default();
    let w = win::find(pid)?;
    win::send_keys(w, &keys)?;
    if let Some(name) = rest.get(1) {
        shot_to(w, name)?;
    }
    Ok(())
}

pub fn shot(args: &[String]) -> R<()> {
    let (pid, rest) = pid_arg(args);
    let w = win::find(pid)?;
    shot_to(w, rest.first().map(String::as_str).unwrap_or("shot"))
}

fn pid_arg(args: &[String]) -> (Option<u32>, Vec<String>) {
    let pid = args.iter().find_map(|a| a.strip_prefix("--pid=")?.parse().ok());
    (pid, args.iter().filter(|a| !a.starts_with("--pid=")).cloned().collect())
}

fn shot_to(w: win::Window, name: &str) -> R<()> {
    let (width, height, rgba) = win::capture(w)?;
    let d = root().join("build/shots");
    std::fs::create_dir_all(&d)?;
    let p = d.join(format!("{name}.png"));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&p)?), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header()?.write_image_data(&rgba)?;
    println!("{} ({width}x{height}): {}", p.display(), win::title(w));
    Ok(())
}

#[cfg(not(windows))]
mod win {
    use crate::util::R;
    #[derive(Clone, Copy)]
    pub struct Window;
    pub fn find(_: Option<u32>) -> R<Window> {
        Err("only on Windows".into())
    }
    pub fn send_keys(_: Window, _: &str) -> R<()> {
        Err("only on Windows".into())
    }
    pub fn capture(_: Window) -> R<(u32, u32, Vec<u8>)> {
        Err("only on Windows".into())
    }
    pub fn title(_: Window) -> String {
        String::new()
    }
}

#[cfg(windows)]
mod win {
    use crate::util::R;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, MapVirtualKeyW, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    #[derive(Clone, Copy)]
    pub struct Window(HWND);

    fn game_pids() -> Vec<u32> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                let n = String::from_utf16_lossy(&e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)]);
                if n.eq_ignore_ascii_case(super::GAME_EXE) {
                    out.push(e.th32ProcessID);
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        out.sort();
        out
    }

    pub fn title(w: Window) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(w.0, buf.as_mut_ptr(), 256) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// The game's main window (of process `pid`, or of the first game process that has one; the
    /// launcher exe also runs as a process without a window).
    pub fn find(pid: Option<u32>) -> R<Window> {
        let pids = game_pids();
        if pids.is_empty() {
            return Err("the game is not running".into());
        }
        let want: Vec<u32> = pid.map(|p| vec![p]).unwrap_or_else(|| pids.clone());
        struct Search(Vec<u32>, HWND);
        unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> i32 {
            let s = &mut *(l as *mut Search);
            let mut p = 0u32;
            GetWindowThreadProcessId(h, &mut p);
            if s.0.contains(&p) && IsWindowVisible(h) != 0 && GetWindowTextLengthW(h) > 0 {
                s.1 = h;
                return 0;
            }
            1
        }
        let mut s = Search(want, std::ptr::null_mut());
        unsafe {
            SetProcessDPIAware();
            EnumWindows(Some(cb), &mut s as *mut Search as LPARAM);
        }
        if s.1.is_null() {
            return Err(format!("no game window (game processes: {pids:?})").into());
        }
        Ok(Window(s.1))
    }

    fn vk(k: &str) -> R<u8> {
        Ok(match k {
            "RETURN" | "ENTER" => 0x0d,
            "ESC" | "ESCAPE" => 0x1b,
            "SPACE" => 0x20,
            "LEFT" => 0x25,
            "UP" => 0x26,
            "RIGHT" => 0x27,
            "DOWN" => 0x28,
            "BACK" => 0x08,
            "TAB" => 0x09,
            "SHIFT" => 0x10,
            "CTRL" => 0x11,
            "ALT" => 0x12,
            "CTRLR" => 0xa3,
            "F1" => 0x70,
            "F2" => 0x71,
            "F5" => 0x74,
            "F11" => 0x7a,
            k if k.len() == 1 => k.as_bytes()[0].to_ascii_uppercase(),
            k => u8::from_str_radix(k, 16).map_err(|_| format!("unknown key {k}"))?,
        })
    }

    /// RETURN,DOWN,WAIT500,…: each key held 80 ms, 300 ms apart.
    pub fn send_keys(w: Window, keys: &str) -> R<()> {
        unsafe {
            ShowWindow(w.0, SW_RESTORE);
            SetForegroundWindow(w.0);
        }
        std::thread::sleep(Duration::from_millis(150));
        for k in keys.split(',').map(|k| k.trim().to_ascii_uppercase()).filter(|k| !k.is_empty()) {
            if let Some(ms) = k.strip_prefix("WAIT") {
                std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(500)));
                continue;
            }
            let code = vk(&k)?;
            let ext = if (0x21..=0x2e).contains(&code) { KEYEVENTF_EXTENDEDKEY } else { 0 };
            unsafe {
                let sc = MapVirtualKeyW(code as u32, 0) as u8;
                keybd_event(code, sc, ext, 0);
                std::thread::sleep(Duration::from_millis(80));
                keybd_event(code, sc, ext | KEYEVENTF_KEYUP, 0);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(())
    }

    /// The client area as RGBA (PrintWindow: works when other windows overlap it).
    pub fn capture(w: Window) -> R<(u32, u32, Vec<u8>)> {
        unsafe {
            let (mut wr, mut cr): (RECT, RECT) = (std::mem::zeroed(), std::mem::zeroed());
            GetWindowRect(w.0, &mut wr);
            GetClientRect(w.0, &mut cr);
            let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            ClientToScreen(w.0, &mut pt);
            let (fw, fh) = (wr.right - wr.left, wr.bottom - wr.top);
            let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);
            if cw <= 0 || ch <= 0 {
                return Err("the window has no client area (minimised?)".into());
            }
            let screen = GetDC(std::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, fw, fh);
            let old = SelectObject(dc, bmp);
            PrintWindow(w.0, dc, 2 as PRINT_WINDOW_FLAGS); // PW_RENDERFULLCONTENT
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = fw;
            info.bmiHeader.biHeight = -fh; // top-down
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut px = vec![0u8; (fw * fh * 4) as usize];
            SelectObject(dc, old);
            GetDIBits(dc, bmp, 0, fh as u32, px.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
            DeleteObject(bmp);
            DeleteDC(dc);
            ReleaseDC(std::ptr::null_mut(), screen);
            let (ox, oy) = (pt.x - wr.left, pt.y - wr.top);
            let mut out = Vec::with_capacity((cw * ch * 4) as usize);
            for y in 0..ch {
                for x in 0..cw {
                    let i = (((oy + y) * fw + ox + x) * 4) as usize;
                    out.extend_from_slice(&[px[i + 2], px[i + 1], px[i], 255]);
                }
            }
            Ok((cw as u32, ch as u32, out))
        }
    }
}
