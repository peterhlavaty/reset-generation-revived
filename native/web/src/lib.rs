//! Reset Generation in a web browser: the recompiled engine (C, built for wasm32 by
//! `cargo x build-web`) with a canvas, the keyboard (and on-screen buttons on touch screens),
//! Web Audio, the game data fetched into the engine's in-memory file system, and saves kept in
//! localStorage. Online play (Quick Match, hosting with F5) goes through the page's WebSockets
//! (link.rs) to the relay: `?server=ws://…/ws` picks another one than the public server.

mod libc;
mod link;
mod saves;

use js_sys::Uint8Array;
use std::cell::RefCell;
use std::ffi::{c_char, c_int, CString};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioContext, AudioContextState, CanvasRenderingContext2d, HtmlCanvasElement, HtmlInputElement, ImageData, KeyboardEvent, PointerEvent, Response};

const W: usize = 640;
const H: usize = 360;
const RATE: f32 = 8000.0; // engine audio: 8 kHz mono s16
const GAME: &str = "/game";

extern "C" {
    fn rg_init(dll: *const c_char, dir: *const c_char, w: c_int, h: c_int, lang: *const c_char) -> c_int;
    fn rg_tick() -> c_int;
    fn rg_pixels() -> *const u32;
    fn rg_key(down: c_int, keycode: c_int);
    fn rg_audio(buf: *mut u8, len: c_int);
    fn rg_use_virtual_clock(on: c_int);
    fn rg_advance_ms(ms: u32);
    fn rg_fs_add(path: *const c_char, data: *const u8, len: usize) -> c_int;
    fn rg_web_clock() -> u32;
    fn rt_mem_init();
    fn rg_net_configure(server: *const c_char, nick: *const c_char);
}

struct Game {
    ctx: CanvasRenderingContext2d,
    rgba: Vec<u8>,
    /// performance.now() + offset = the engine clock it should have reached
    offset: Option<f64>,
    last_now: f64,
    audio: Option<AudioContext>,
    audio_at: f64,
    pending: Vec<f32>,
    audio_debt: f64,
    held: Vec<c_int>,
    frame: u32, // engine frames run
    test: Test,
}

thread_local! {
    static GAME_STATE: RefCell<Option<Game>> = const { RefCell::new(None) };
}

fn window() -> web_sys::Window {
    web_sys::window().expect("no window")
}

pub(crate) fn now_ms() -> f64 {
    window().performance().map(|p| p.now()).unwrap_or(0.0)
}

fn status(msg: &str) {
    if let Some(e) = window().document().and_then(|d| d.get_element_by_id("status")) {
        e.set_text_content(Some(msg));
        let _ = e.set_attribute("style", if msg.is_empty() { "display:none" } else { "" });
    }
}

pub(crate) fn show_error(msg: &str) {
    web_sys::console::error_1(&msg.into());
    status(msg);
}

#[wasm_bindgen(start)]
pub fn start() {
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run().await {
            show_error(&format!("Cannot start the game: {}", e.as_string().unwrap_or_else(|| format!("{e:?}"))));
        }
    });
}

async fn fetch(url: &str) -> Result<Response, JsValue> {
    let r: Response = JsFuture::from(window().fetch_with_str(url)).await?.dyn_into()?;
    if !r.ok() {
        return Err(format!("{url}: HTTP {}", r.status()).into());
    }
    Ok(r)
}

fn add_file(path: &str, data: &[u8]) -> Result<(), JsValue> {
    let p = CString::new(path).map_err(|_| "bad file name")?;
    if unsafe { rg_fs_add(p.as_ptr(), data.as_ptr(), data.len()) } == 0 {
        return Err(format!("out of memory for {path}").into());
    }
    Ok(())
}

async fn run() -> Result<(), JsValue> {
    unsafe { rt_mem_init() }; // the guest memory first, at its fixed addresses (native/rt/host.h)
    // the game files: data/files.txt lists "<name> <size>" lines (written by `cargo x build-web`)
    let list = JsFuture::from(fetch("data/files.txt").await?.text()?).await?.as_string().unwrap_or_default();
    let files: Vec<(String, f64)> = list
        .lines()
        .filter_map(|l| {
            let (n, s) = l.trim().rsplit_once(' ')?;
            Some((n.to_string(), s.parse().ok()?))
        })
        .collect();
    let total: f64 = files.iter().map(|f| f.1).sum::<f64>().max(1.0);
    let mut done = 0.0;
    for (name, size) in &files {
        status(&format!("Loading the game... {:.0} %", done * 100.0 / total));
        let buf = JsFuture::from(fetch(&format!("data/{name}")).await?.array_buffer()?).await?;
        add_file(&format!("{GAME}/{name}"), &Uint8Array::new(&buf).to_vec())?;
        done += size;
    }
    let kept = saves::all();
    for (path, data) in &kept {
        add_file(path, data)?;
    }
    if !kept.is_empty() {
        web_sys::console::log_1(&format!("rg: {} saved files from this browser", kept.len()).into());
    }
    status("Starting...");

    let doc = window().document().ok_or("no document")?;
    let canvas: HtmlCanvasElement = doc.get_element_by_id("screen").ok_or("no canvas")?.dyn_into()?;
    canvas.set_width(W as u32);
    canvas.set_height(H as u32);
    let ctx: CanvasRenderingContext2d = canvas.get_context("2d")?.ok_or("no 2d canvas")?.dyn_into()?;

    online(&doc)?;
    let c = |s: &str| CString::new(s).unwrap();
    unsafe {
        rg_use_virtual_clock(1); // see frame()
        if rg_init(c(&format!("{GAME}/ResetGeneration-Win32-Web.dll")).as_ptr(), c(GAME).as_ptr(), W as c_int, H as c_int, c("en").as_ptr()) == 0 {
            return Err("the engine did not start (see the browser console)".into());
        }
    }
    GAME_STATE.with(|g| {
        *g.borrow_mut() = Some(Game {
            ctx,
            rgba: vec![255; W * H * 4],
            offset: None,
            last_now: now_ms(),
            audio: None,
            audio_at: 0.0,
            pending: Vec::new(),
            audio_debt: 0.0,
            held: Vec::new(),
            frame: 0,
            test: test_spec(),
        })
    });
    status("");
    input(&doc)?;
    animate();
    Ok(())
}

/// The relay and this player's name: `?server=` and `?nick=` from the address, else the name
/// field's (kept in localStorage; made up at first, and then kept, so the relay knows this player
/// when they come back to a match).
fn online(doc: &web_sys::Document) -> Result<(), JsValue> {
    let storage = window().local_storage().ok().flatten();
    let mut nick = clean_nick(&param("nick"));
    if nick.is_empty() {
        nick = clean_nick(&storage.as_ref().and_then(|s| s.get_item(NICK).ok().flatten()).unwrap_or_default());
    }
    if nick.is_empty() {
        nick = format!("Player{}", 1000 + (js_sys::Math::random() * 9000.0) as u32);
    }
    if let Some(s) = &storage {
        let _ = s.set_item(NICK, &nick);
    }
    let server = param("server");
    let c = |s: &str| CString::new(s).unwrap_or_default();
    let server = c(if server.is_empty() { "online" } else { &server });
    unsafe { rg_net_configure(server.as_ptr(), c(&nick).as_ptr()) };
    let Some(field) = doc.get_element_by_id("nick").and_then(|e| e.dyn_into::<HtmlInputElement>().ok()) else { return Ok(()) };
    field.set_value(&nick);
    let f = field.clone();
    let change = Closure::<dyn FnMut()>::new(move || {
        let nick = clean_nick(&f.value());
        if nick.is_empty() {
            return;
        }
        f.set_value(&nick);
        if let Some(s) = window().local_storage().ok().flatten() {
            let _ = s.set_item(NICK, &nick);
        }
        let n = CString::new(nick).unwrap_or_default();
        unsafe { rg_net_configure(std::ptr::null(), n.as_ptr()) }; // for the next login
    });
    field.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    let f = field.clone();
    let done = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        if e.key() == "Enter" || e.key() == "Escape" {
            let _ = f.blur(); // back to the game
        }
    });
    field.add_event_listener_with_callback("keydown", done.as_ref().unchecked_ref())?;
    done.forget();
    Ok(())
}

const NICK: &str = "rg-nick";

/// The relay's rule for names: 1 to 15 letters, digits or _.
fn clean_nick(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(15).collect()
}

/// A parameter of the page address (`?name=value&…`), "" if it is not there.
fn param(name: &str) -> String {
    let search = window().location().search().unwrap_or_default();
    let raw = search.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='));
    raw.map(|s| js_sys::decode_uri_component(s).ok().and_then(|s| s.as_string()).unwrap_or_else(|| s.to_string())).unwrap_or_default()
}

/// Java AWT key codes (engine.h) for KeyboardEvent.code
fn awt(code: &str) -> Option<c_int> {
    Some(match code {
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Enter" | "NumpadEnter" => 10,
        "Escape" | "Backspace" => 27,
        "ControlLeft" | "ControlRight" => 17,
        "Space" => 32,
        "F5" => 116, // host a game (native/rt/server.c), not "reload"
        _ => {
            let b = code.as_bytes();
            match (code.strip_prefix("Key"), code.strip_prefix("Digit")) {
                (Some(k), _) if k.len() == 1 => b[3].to_ascii_uppercase() as c_int,
                (_, Some(d)) if d.len() == 1 => b[5] as c_int,
                _ => return None,
            }
        }
    })
}

fn key(down: bool, code: c_int) {
    GAME_STATE.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.as_mut() else { return };
        if down {
            unlock_audio(g);
            if g.held.contains(&code) {
                return;
            }
            g.held.push(code);
        } else {
            let Some(i) = g.held.iter().position(|&k| k == code) else { return };
            g.held.remove(i);
        }
        unsafe { rg_key(down as c_int, code) };
    });
}

fn release_all() {
    let held = GAME_STATE.with(|g| g.borrow().as_ref().map(|g| g.held.clone()).unwrap_or_default());
    for k in held {
        key(false, k);
    }
}

fn input(doc: &web_sys::Document) -> Result<(), JsValue> {
    let w = window();
    for (ev, down) in [("keydown", true), ("keyup", false)] {
        let f = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            if e.alt_key() || e.meta_key() || e.target().is_some_and(|t| t.is_instance_of::<HtmlInputElement>()) {
                return; // browser shortcuts, typing a name
            }
            if let Some(code) = awt(&e.code()) {
                e.prevent_default();
                if !e.repeat() {
                    key(down, code);
                }
            }
        });
        w.add_event_listener_with_callback(ev, f.as_ref().unchecked_ref())?;
        f.forget();
    }
    let blur = Closure::<dyn FnMut()>::new(release_all);
    w.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())?;
    blur.forget();
    // sound may only start after the player did something on the page
    let tap = Closure::<dyn FnMut()>::new(|| GAME_STATE.with(|g| g.borrow_mut().as_mut().map(unlock_audio)).unwrap_or(()));
    w.add_event_listener_with_callback("pointerdown", tap.as_ref().unchecked_ref())?;
    tap.forget();
    // on-screen buttons (touch screens): elements with data-key="<AWT code>"
    let buttons = doc.query_selector_all("[data-key]")?;
    for i in 0..buttons.length() {
        let Some(el) = buttons.item(i).and_then(|n| n.dyn_into::<web_sys::Element>().ok()) else { continue };
        let Some(code) = el.get_attribute("data-key").and_then(|k| k.parse::<c_int>().ok()) else { continue };
        for (ev, down) in [("pointerdown", true), ("pointerup", false), ("pointercancel", false), ("pointerleave", false)] {
            let f = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
                e.prevent_default();
                key(down, code);
            });
            el.add_event_listener_with_callback(ev, f.as_ref().unchecked_ref())?;
            f.forget();
        }
    }
    Ok(())
}

fn unlock_audio(g: &mut Game) {
    match &g.audio {
        Some(a) if a.state() == AudioContextState::Suspended => {
            let _ = a.resume();
        }
        Some(_) => {}
        None => g.audio = AudioContext::new().ok(),
    }
}

fn animate() {
    let cb: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>> = Rc::new(RefCell::new(None));
    let again = cb.clone();
    *cb.borrow_mut() = Some(Closure::new(move |now: f64| {
        if GAME_STATE.with(|g| g.borrow_mut().as_mut().map(|g| frame(g, now)).unwrap_or(false)) {
            let _ = window().request_animation_frame(again.borrow().as_ref().unwrap().as_ref().unchecked_ref());
        } else {
            status("The game has ended. Reload the page to play again.");
        }
    }));
    let _ = window().request_animation_frame(cb.borrow().as_ref().unwrap().as_ref().unchecked_ref());
}

/// One animation frame: run the engine up to the page's clock, show the picture, queue the
/// sound. The engine runs on its virtual clock, as in the test driver (rgtest): 1 ms a tick
/// plus what it sleeps (pacing itself to 32 fps), never blocking the page. false when the game
/// asked to quit.
fn frame(g: &mut Game, now: f64) -> bool {
    let clock = unsafe { rg_web_clock() };
    let offset = *g.offset.get_or_insert(clock as f64 - now);
    let target = (now + offset) as u32;
    let mut ticks = 0;
    while (target.wrapping_sub(unsafe { rg_web_clock() }) as i32) > 0 && ticks < 8 {
        test_keys(g);
        unsafe { rg_advance_ms(1) };
        if unsafe { rg_tick() } == 0 {
            return false;
        }
        if g.test.active && g.frame % 2 == 0 {
            let mut pcm = [0u8; 1024]; // rgtest's audio pulls: the engine mixes in guest code
            unsafe { rg_audio(pcm.as_mut_ptr(), pcm.len() as c_int) };
        }
        test_shot(g);
        ticks += 1;
        g.frame += 1;
    }
    // far behind (a hidden tab, a slow device): carry on from here instead of racing to catch up
    let behind = target.wrapping_sub(unsafe { rg_web_clock() }) as i32;
    if behind > 250 {
        g.offset = Some(offset - behind as f64);
    }
    if ticks > 0 {
        show(g);
    }
    sound(g, now);
    true
}

fn show(g: &mut Game) {
    let px = unsafe { std::slice::from_raw_parts(rg_pixels(), W * H) };
    for (o, p) in g.rgba.chunks_exact_mut(4).zip(px) {
        o[0] = (p >> 16) as u8;
        o[1] = (p >> 8) as u8;
        o[2] = *p as u8;
    }
    if let Ok(img) = ImageData::new_with_u8_clamped_array_and_sh(Clamped(&g.rgba), W as u32, H as u32) {
        let _ = g.ctx.put_image_data(&img, 0.0, 0.0);
    }
}

/// A test aid like rgtest's --keys/--shots, from the page address: `?keys=F:KEY,…&shots=F,…`
/// presses keys (ENTER ESC UP DOWN LEFT RIGHT SPACE CTRL or an AWT code; held 6 frames, or N
/// with F:KEY*N) before engine frame F and sends a screenshot after frame F to
/// `cargo x serve-web` (build/shots/web-FFFFF.png; the frames are rgtest's: compare them with
/// `cargo x img-diff`).
struct Test {
    keys: Vec<(u32, bool, c_int)>, // frame, down, code; the next one last
    shots: Vec<u32>,
    active: bool, // keys or shots given: sound is pulled like rgtest does (same frames)
}

fn test_spec() -> Test {
    let key = |k: &str| match k.to_ascii_uppercase().as_str() {
        "ENTER" | "RETURN" => Some(10),
        "ESC" => Some(27),
        "UP" => Some(38),
        "DOWN" => Some(40),
        "LEFT" => Some(37),
        "RIGHT" => Some(39),
        "SPACE" => Some(32),
        "CTRL" => Some(17),
        n => n.parse().ok(),
    };
    let mut keys = Vec::new();
    for p in param("keys").split(',') {
        let Some((f, k)) = p.split_once(':') else { continue };
        let (k, hold) = k.split_once('*').map_or((k, Some(6)), |(k, n)| (k, n.parse().ok()));
        if let (Ok(f), Some(code), Some(hold)) = (f.parse::<u32>(), key(k), hold) {
            keys.push((f, true, code));
            keys.push((f + hold, false, code));
        }
    }
    keys.sort_by_key(|k| std::cmp::Reverse((k.0, k.1)));
    let mut shots: Vec<u32> = param("shots").split(',').filter_map(|s| s.parse().ok()).collect();
    shots.sort_by_key(|&s| std::cmp::Reverse(s));
    let active = !keys.is_empty() || !shots.is_empty();
    Test { keys, shots, active }
}

fn test_keys(g: &mut Game) {
    while let Some(&(_, down, code)) = g.test.keys.last().filter(|k| k.0 <= g.frame) {
        g.test.keys.pop();
        unsafe { rg_key(down as c_int, code) };
    }
}

fn test_shot(g: &mut Game) {
    if g.test.shots.last().is_some_and(|&s| s <= g.frame) {
        g.test.shots.pop();
        show(g);
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&Uint8Array::from(&g.rgba[..]));
        let _ = window().fetch_with_str_and_init(&format!("/shot/web-{:05}", g.frame), &init);
    }
}

/// Pulls as much engine sound as time passed (8 samples a millisecond) and plays it in 50 ms
/// pieces, about 0.1 s ahead.
fn sound(g: &mut Game, now: f64) {
    if g.test.active {
        return;
    }
    let elapsed = (now - g.last_now).clamp(0.0, 250.0);
    g.last_now = now;
    g.audio_debt += elapsed * RATE as f64 / 1000.0;
    let n = g.audio_debt as usize & !1;
    if n == 0 {
        return;
    }
    g.audio_debt -= n as f64;
    let mut pcm = vec![0u8; n * 2];
    unsafe { rg_audio(pcm.as_mut_ptr(), pcm.len() as c_int) };
    let Some(a) = g.audio.as_ref().filter(|a| a.state() == AudioContextState::Running) else {
        g.pending.clear();
        return;
    };
    g.pending.extend(pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0));
    if g.pending.len() < 400 {
        return;
    }
    let t = a.current_time();
    if g.audio_at < t + 0.02 {
        g.audio_at = t + 0.08; // (re)start with a little slack
    }
    if let Ok(buf) = a.create_buffer(1, g.pending.len() as u32, RATE) {
        let _ = buf.copy_to_channel(&g.pending, 0);
        if let Ok(src) = a.create_buffer_source() {
            src.set_buffer(Some(&buf));
            let _ = src.connect_with_audio_node(&a.destination());
            let _ = src.start_with_when(g.audio_at);
            g.audio_at += g.pending.len() as f64 / RATE as f64;
        }
    }
    g.pending.clear();
}
