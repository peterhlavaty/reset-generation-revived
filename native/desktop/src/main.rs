//! Reset Generation as a native desktop program: the recompiled engine (C, linked by build.rs)
//! in a minifb window with tinyaudio sound. Everything it needs sits next to the executable:
//! `data/` (the game data, patched for the 640x360 engine size) and `revived.ini`.
#![cfg_attr(windows, windows_subsystem = "windows")]

use minifb::{Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const W: usize = 640;
const H: usize = 360;
const ONLINE: &str = "ws://resetgeneration.rustyapps.com/ws";

extern "C" {
    fn rg_desktop_init(log_path: *const c_char);
    fn rg_net_configure(server: *const c_char, nick: *const c_char);
    fn rg_init(dll: *const c_char, dir: *const c_char, w: c_int, h: c_int, lang: *const c_char) -> c_int;
    fn rg_tick() -> c_int;
    fn rg_pixels() -> *const u32;
    fn rg_key(down: c_int, keycode: c_int);
    fn rg_audio(buf: *mut u8, len: c_int);
    fn rg_host_configure(players: c_int, hero: c_int);
    fn rg_host_start(players: c_int, hero: c_int) -> c_int;
    fn rg_host_status() -> *const c_char;
    fn rg_host_players() -> c_int;
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

/// revived.ini next to the executable (written with defaults on the first start), overridden by
/// `--key=value` arguments.
struct Config {
    server: String,
    nickname: String,
    scale: String,
    /// hosting (F5): player count, hero (a name or id, RANDOM, or empty: choose in the game)
    host_players: i32,
    host_hero: String,
    /// test aids: host=auto hosts by itself, keys=T:KEY,... presses keys T ms after the start
    host: String,
    keys: String,
}

impl Config {
    fn load(dir: &Path) -> Config {
        let path = dir.join("revived.ini");
        let mut c = Config {
            server: "online".into(),
            nickname: String::new(),
            scale: "fit".into(),
            host_players: 2,
            host_hero: String::new(),
            host: String::new(),
            keys: String::new(),
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        c.apply(text.lines());
        if c.nickname.is_empty() {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7);
            c.nickname = format!("Player{}", 1000 + t % 9000);
        }
        let args: Vec<String> = std::env::args().skip(1).filter_map(|a| a.strip_prefix("--").map(String::from)).collect();
        if text.is_empty() {
            let _ = std::fs::write(
                &path,
                format!(
                    "# Reset Generation (native)\n\
                     # server: online (the public server) or ws://HOST:PORT/ws\n\
                     server={}\n\
                     # your name in online games (letters, digits, _)\n\
                     nickname={}\n\
                     # window size: fit (largest that fits the screen), 1, 2 or 4\n\
                     scale={}\n\
                     # hosting a game (F5 in the main menu): players 2-4 (remembered), hero (a name,\n\
                     # RANDOM, or empty: choose in the game)\n\
                     host_players={}\n\
                     host_hero=\n",
                    c.server, c.nickname, c.scale, c.host_players
                ),
            );
        }
        c.apply(args.iter().map(String::as_str));
        c
    }

    fn apply<'a>(&mut self, lines: impl Iterator<Item = &'a str>) {
        for line in lines {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.split('#').next().unwrap_or("").trim().to_string();
            match k.trim() {
                "server" if !v.is_empty() => self.server = v,
                "nickname" => {
                    let n: String = v.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_').take(16).collect();
                    if !n.is_empty() {
                        self.nickname = n;
                    }
                }
                "scale" if !v.is_empty() => self.scale = v,
                "host_players" => self.host_players = v.parse().unwrap_or(2),
                "host_hero" => self.host_hero = v.to_ascii_uppercase(),
                "host" => self.host = v,
                "keys" => self.keys = v,
                _ => {}
            }
        }
    }

    /// host_hero: a hero name or id -> 0..9, RANDOM -> -1, empty -> -2 (choose in the game).
    fn hero(&self) -> c_int {
        const HEROES: [&str; 10] = [
            "HEDGEHOG", "PLUMBER", "CYBORG", "LEVEL_50_ELF", "BABE_GUNNER", "MONSTER_TRAINER", "SCIFI_KNIGHT", "NINJA", "DR_LOVEBOMBER",
            "AGGRESSOR",
        ];
        let h = self.host_hero.as_str();
        if h == "RANDOM" {
            return -1;
        }
        HEROES.iter().position(|n| *n == h).or_else(|| h.parse().ok().filter(|i| *i < 10)).map_or(-2, |i| i as c_int)
    }

    /// Sets `key=value` in revived.ini (replacing the line, or appending it).
    fn save(dir: &Path, key: &str, value: &str) {
        let path = dir.join("revived.ini");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let line = format!("{key}={value}");
        let mut found = false;
        let mut out: Vec<String> = text
            .lines()
            .map(|l| if l.split_once('=').is_some_and(|(k, _)| k.trim() == key) { found = true; line.clone() } else { l.to_string() })
            .collect();
        if !found {
            out.push(line);
        }
        let _ = std::fs::write(&path, out.join("
") + "
");
    }

    fn server_url(&self) -> &str {
        match self.server.as_str() {
            "online" | "auto" => ONLINE,
            s => s,
        }
    }

    fn scale(&self) -> Scale {
        match self.scale.as_str() {
            "1" => Scale::X1,
            "2" => Scale::X2,
            "4" => Scale::X4,
            _ => Scale::FitScreen,
        }
    }
}

/// Java AWT key codes, which the engine expects.
fn awt(k: Key) -> Option<c_int> {
    Some(match k {
        Key::Left => 37,
        Key::Up => 38,
        Key::Right => 39,
        Key::Down => 40,
        Key::Enter | Key::NumPadEnter => 10,
        Key::Escape | Key::Backspace => 27,
        Key::LeftCtrl | Key::RightCtrl => 17,
        Key::Space => 32,
        Key::F5 => 116,
        k if (Key::A as u32..=Key::Z as u32).contains(&(k as u32)) => 65 + (k as u32 - Key::A as u32) as c_int,
        k if (Key::Key0 as u32..=Key::Key9 as u32).contains(&(k as u32)) => 48 + (k as u32 - Key::Key0 as u32) as c_int,
        _ => return None,
    })
}

/// keys=T:KEY,... (a test aid): AWT key codes and when to press them (ms after the start).
fn parse_keys(spec: &str) -> Vec<(u128, c_int)> {
    let mut out: Vec<(u128, c_int)> = spec
        .split(',')
        .filter_map(|p| {
            let (t, k) = p.trim().split_once(':')?;
            let code = match k.trim().to_ascii_uppercase().as_str() {
                "ESC" => 27,
                "RETURN" | "ENTER" => 10,
                "SPACE" => 32,
                "CTRL" => 17,
                "LEFT" => 37,
                "UP" => 38,
                "RIGHT" => 39,
                "DOWN" => 40,
                "F5" => 116,
                k => i32::from_str_radix(k, 16).ok()?,
            };
            Some((t.trim().parse().ok()?, code))
        })
        .collect();
    out.sort();
    out
}

/// The engine's sound (8 kHz mono) waits here for the audio thread, which resamples it.
type Ring = Arc<Mutex<VecDeque<i16>>>;

fn start_audio(ring: Ring) -> Option<tinyaudio::OutputDevice> {
    let params = tinyaudio::OutputDeviceParameters { sample_rate: 44100, channels_count: 2, channel_sample_count: 1024 };
    let step = 8000.0 / params.sample_rate as f64;
    let mut pos = 0.0f64;
    let dev = tinyaudio::run_output_device(params, move |out: &mut [f32]| {
        let mut q = ring.lock().unwrap();
        for frame in out.chunks_mut(2) {
            let v = if q.len() >= 2 {
                let (a, b) = (q[0] as f64, q[1] as f64);
                let v = a + (b - a) * pos;
                pos += step;
                while pos >= 1.0 {
                    pos -= 1.0;
                    q.pop_front();
                }
                (v / 32768.0) as f32
            } else {
                0.0
            };
            frame.fill(v);
        }
    });
    match dev {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("no sound: {e}");
            None
        }
    }
}

fn main() {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| ".".into());
    let data = exe_dir.join("data");
    let dll = data.join("ResetGeneration-Win32-Web.dll");
    let log = exe_dir.join("ResetGeneration.log");
    unsafe { rg_desktop_init(cstr(&log.to_string_lossy()).as_ptr()) };
    let cfg = Config::load(&exe_dir);
    unsafe { rg_net_configure(cstr(cfg.server_url()).as_ptr(), cstr(&cfg.nickname).as_ptr()) };

    // The engine's file API is ANSI and gets its home directory as given: run from inside it with
    // a relative path, so a game folder with non-ASCII characters works too.
    if std::env::set_current_dir(&data).is_err() || !dll.is_file() {
        fatal(&format!("game data not found in {}", data.display()));
    }
    let dll_name = cstr("ResetGeneration-Win32-Web.dll");
    if unsafe { rg_init(dll_name.as_ptr(), cstr(".").as_ptr(), W as c_int, H as c_int, cstr("en").as_ptr()) } == 0 {
        fatal("the engine failed to start (see ResetGeneration.log)");
    }

    let opts = WindowOptions { resize: true, scale: cfg.scale(), scale_mode: ScaleMode::AspectRatioStretch, ..WindowOptions::default() };
    let mut win = Window::new("Reset Generation", W, H, opts).unwrap_or_else(|e| fatal(&format!("cannot open a window: {e}")));
    win.set_target_fps(0); // the engine paces itself (32 fps)

    let ring: Ring = Arc::new(Mutex::new(VecDeque::with_capacity(4096)));
    let _audio = start_audio(ring.clone());
    let mut frame = vec![0u32; W * H];
    let mut pcm = [0u8; 512];
    unsafe { rg_host_configure(cfg.host_players, cfg.hero()) };
    let mut keys = parse_keys(&cfg.keys);
    let start = std::time::Instant::now();
    let (mut title, mut players, mut next_auto) = (String::new(), cfg.host_players, 15000u128);
    while win.is_open() {
        let t = start.elapsed().as_millis();
        while keys.first().is_some_and(|k| k.0 <= t) {
            let (_, code) = keys.remove(0);
            unsafe {
                rg_key(1, code);
                rg_key(0, code);
            }
        }
        if cfg.host == "auto" && t >= next_auto {
            next_auto = if unsafe { rg_host_start(cfg.host_players, cfg.hero()) } != 0 { u128::MAX } else { t + 2000 };
        }
        let status = unsafe { std::ffi::CStr::from_ptr(rg_host_status()) }.to_string_lossy().into_owned();
        if status != title {
            win.set_title(&if status.is_empty() { "Reset Generation".to_string() } else { format!("Reset Generation - {status}") });
            title = status;
        }
        let p = unsafe { rg_host_players() };
        if p != players {
            players = p;
            Config::save(&exe_dir, "host_players", &p.to_string());
        }
        for k in win.get_keys_pressed(KeyRepeat::No) {
            if let Some(c) = awt(k) {
                unsafe { rg_key(1, c) };
            }
        }
        for k in win.get_keys_released() {
            if let Some(c) = awt(k) {
                unsafe { rg_key(0, c) };
            }
        }
        if unsafe { rg_tick() } == 0 {
            break;
        }
        frame.copy_from_slice(unsafe { std::slice::from_raw_parts(rg_pixels(), W * H) });
        if win.update_with_buffer(&frame, W, H).is_err() {
            break;
        }
        // keep about 100 ms of sound queued
        while ring.lock().unwrap().len() < 800 {
            unsafe { rg_audio(pcm.as_mut_ptr(), pcm.len() as c_int) };
            let mut q = ring.lock().unwrap();
            q.extend(pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        }
    }
}

fn fatal(msg: &str) -> ! {
    // host_fatal logs it and, on Windows, shows it in a message box
    extern "C" {
        fn host_fatal(msg: *const c_char) -> !;
    }
    unsafe { host_fatal(cstr(msg).as_ptr()) }
}
