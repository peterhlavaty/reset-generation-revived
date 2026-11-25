//! The relay for LAN games, run inside a hosting game (`server=lan`, or `auto` without internet):
//! the same server as the public one (`server/`), listening on all interfaces on port 8787 and
//! answering LAN discovery there (UDP). It ends with the game.

use std::ffi::{c_char, CStr, CString};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PORT: u16 = 8787;

fn log(msg: &str) {
    extern "C" {
        fn host_log(fmt: *const c_char, ...);
    }
    let m = CString::new(msg.replace('\0', "")).unwrap();
    unsafe { host_log(c"%s".as_ptr(), m.as_ptr()) };
}

/// Whether a relay answers GET /status on this PC.
fn local_relay_up() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    if s.write_all(b"GET /status HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
        return false;
    }
    let mut r = String::new();
    let _ = s.read_to_string(&mut r);
    r.starts_with("HTTP/1.") && r.contains("\"players\"")
}

/// Called by the engine host (native/rt/link.c, on its relay thread): the address of this PC's
/// LAN relay, started on the first call; NULL if port 8787 is taken by something else.
pub extern "C" fn start_relay(nick: *const c_char) -> *const c_char {
    static URL: Mutex<Option<CString>> = Mutex::new(None);
    let mut url = URL.lock().unwrap();
    if url.is_none() {
        if !local_relay_up() {
            let mut name = if nick.is_null() { String::new() } else { unsafe { CStr::from_ptr(nick) }.to_string_lossy().into_owned() };
            name.retain(|c| c.is_ascii_graphic());
            let opts = rg_server::net::Options {
                listen: SocketAddr::from(([0, 0, 0, 0], PORT)),
                discovery: Some(if name.is_empty() { "Host".into() } else { name }),
                max_connections: 256,
                cfg: rg_server::relay::Config::default(),
                quiet: true,
            };
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
                let local = tokio::task::LocalSet::new();
                if let Err(e) = local.block_on(&rt, rg_server::net::serve(opts)) {
                    log(&format!("lan relay: {e}"));
                }
            });
            let t = Instant::now();
            while !local_relay_up() {
                if t.elapsed() > Duration::from_secs(5) {
                    log(&format!("lan relay: port {PORT} is in use by another program"));
                    return std::ptr::null();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            log(&format!("lan relay: running on port {PORT} (LAN games)"));
        } else {
            log(&format!("lan relay: using the relay already running on this PC"));
        }
        *url = Some(CString::new(format!("ws://127.0.0.1:{PORT}/ws")).unwrap());
    }
    url.as_ref().unwrap().as_ptr()
}
