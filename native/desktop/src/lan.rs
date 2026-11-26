//! The relay for LAN games, run inside a hosting game (`server=lan`, or `auto` without internet):
//! the same server as the public one (`server/`), listening on all interfaces on port 8787 and
//! answering LAN discovery there (UDP). It ends with the game. A second game on the same PC hosts
//! through the first one's relay, and runs its own once that game has quit.

use std::ffi::{c_char, CStr, CString};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const PORT: u16 = 8787;

/// Whether this game's own relay is running (cleared if its server stops).
static OWN: AtomicBool = AtomicBool::new(false);

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

/// Called by the engine host (native/rt/link.c, on its relay thread) for every connection it
/// opens: the address of this PC's LAN relay, started if none runs; NULL if port 8787 is taken by
/// something else. Another game's relay is asked again each time, because it ends with that game:
/// the hosting game then reconnects and gets a relay of its own here, instead of retrying a dead
/// address for as long as it hosts.
pub extern "C" fn start_relay(nick: *const c_char) -> *const c_char {
    static URL: OnceLock<CString> = OnceLock::new();
    /// the relay in use is another game's (so the log says it once, not on every reconnect)
    static SHARED: Mutex<bool> = Mutex::new(false);
    let url = URL.get_or_init(|| CString::new(format!("ws://127.0.0.1:{PORT}/ws")).unwrap()).as_ptr();
    let mut shared = SHARED.lock().unwrap();
    if OWN.load(Ordering::SeqCst) {
        return url;
    }
    if local_relay_up() {
        if !*shared {
            log("lan relay: using the relay already running on this PC");
            *shared = true;
        }
        return url;
    }
    // Bound here rather than in the relay's thread, so a taken port is known at once.
    let listener = match TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], PORT))) {
        Ok(l) => l,
        Err(e) => {
            // another game on this PC may be starting its relay at this very moment
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(2) {
                if local_relay_up() {
                    log("lan relay: using the relay already running on this PC");
                    *shared = true;
                    return url;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            log(&format!("lan relay: port {PORT} is in use by another program ({e})"));
            return std::ptr::null();
        }
    };
    let mut name = if nick.is_null() { String::new() } else { unsafe { CStr::from_ptr(nick) }.to_string_lossy().into_owned() };
    name.retain(|c| c.is_ascii_graphic());
    let opts = rg_server::net::Options {
        listen: SocketAddr::from(([0, 0, 0, 0], PORT)),
        discovery: Some(if name.is_empty() { "Host".into() } else { name }),
        max_connections: 256,
        cfg: rg_server::relay::Config::default(),
        quiet: true,
    };
    OWN.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        // cleared however the thread ends (a panic too): the next game takes the relay up again
        struct Down;
        impl Drop for Down {
            fn drop(&mut self) {
                OWN.store(false, Ordering::SeqCst);
            }
        }
        let _down = Down;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        let local = tokio::task::LocalSet::new();
        let r = local.block_on(&rt, async move {
            listener.set_nonblocking(true)?; // tokio takes it over as it is
            let addr = listener.local_addr()?;
            rg_server::net::serve_on(tokio::net::TcpListener::from_std(listener)?, opts, addr).await
        });
        if let Err(e) = r {
            log(&format!("lan relay: {e}"));
        }
    });
    log(&format!("lan relay: running on port {PORT} (LAN games)"));
    *shared = false;
    url
}
