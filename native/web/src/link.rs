//! The relay links of the browser build (native/rt/link.h, the RT_NO_THREADS part): the page's
//! WebSockets. Nothing blocks: `link_open` returns at once, the browser connects and queues the
//! frames that arrive between animation frames, and net.c / server.c poll them on the game thread.
//! A link is a small number to C (it never looks inside).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

const CONNECTING: i32 = 0;
const OPEN: i32 = 1;
const CLOSED: i32 = -1;

struct Link {
    ws: WebSocket,
    state: Rc<Cell<i32>>,
    inbox: Rc<RefCell<VecDeque<(c_int, Vec<u8>)>>>,
    refs: u32,
    _handlers: Vec<Closure<dyn FnMut(JsValue)>>,
}

thread_local! {
    static LINKS: RefCell<HashMap<usize, Link>> = RefCell::new(HashMap::new());
    static NEXT: Cell<usize> = const { Cell::new(1) };
}

fn log(msg: &str) {
    web_sys::console::log_1(&msg.into());
}

fn with<T>(l: *mut c_void, f: impl FnOnce(&mut Link) -> T) -> Option<T> {
    LINKS.with(|m| m.borrow_mut().get_mut(&(l as usize)).map(f))
}

/// # Safety
/// `url` and `who` are NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn link_open(url: *const c_char, who: *const c_char) -> *mut c_void {
    let url = unsafe { CStr::from_ptr(url) }.to_string_lossy().into_owned();
    let who = unsafe { CStr::from_ptr(who) }.to_string_lossy().into_owned();
    let ws = match WebSocket::new(&url) {
        Ok(ws) => ws,
        Err(e) => {
            log(&format!("{who}: cannot connect to {url}: {e:?}"));
            return std::ptr::null_mut();
        }
    };
    ws.set_binary_type(BinaryType::Arraybuffer);
    let state = Rc::new(Cell::new(CONNECTING));
    let inbox: Rc<RefCell<VecDeque<(c_int, Vec<u8>)>>> = Rc::default();
    let mut handlers = Vec::new();
    {
        let state = state.clone();
        let f = Closure::<dyn FnMut(JsValue)>::new(move |_| state.set(OPEN));
        ws.set_onopen(Some(f.as_ref().unchecked_ref()));
        handlers.push(f);
    }
    {
        let inbox = inbox.clone();
        let f = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let Ok(e) = e.dyn_into::<MessageEvent>() else { return };
            let data = e.data();
            if let Some(s) = data.as_string() {
                inbox.borrow_mut().push_back((1, s.into_bytes()));
            } else if let Ok(buf) = data.dyn_into::<js_sys::ArrayBuffer>() {
                inbox.borrow_mut().push_back((2, js_sys::Uint8Array::new(&buf).to_vec()));
            }
        });
        ws.set_onmessage(Some(f.as_ref().unchecked_ref()));
        handlers.push(f);
    }
    {
        let (state, who, url) = (state.clone(), who.clone(), url.clone());
        let f = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            if state.get() != CLOSED {
                let why = e.dyn_ref::<CloseEvent>().map(|c| format!("code {}", c.code())).unwrap_or_default();
                log(&format!("{who}: connection to {url} ended {why}"));
            }
            state.set(CLOSED);
        });
        ws.set_onclose(Some(f.as_ref().unchecked_ref()));
        handlers.push(f);
    }
    {
        let state = state.clone();
        let f = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            if e.is_instance_of::<Event>() && state.get() == CONNECTING {
                log(&format!("{who}: cannot connect to {url}"));
            }
            state.set(CLOSED);
        });
        ws.set_onerror(Some(f.as_ref().unchecked_ref()));
        handlers.push(f);
    }
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    LINKS.with(|m| m.borrow_mut().insert(id, Link { ws, state, inbox, refs: 1, _handlers: handlers }));
    id as *mut c_void
}

#[no_mangle]
pub extern "C" fn link_ref(l: *mut c_void) {
    with(l, |k| k.refs += 1);
}

#[no_mangle]
pub extern "C" fn link_release(l: *mut c_void) {
    let last = with(l, |k| {
        k.refs -= 1;
        k.refs == 0
    });
    if last == Some(true) {
        if let Some(k) = LINKS.with(|m| m.borrow_mut().remove(&(l as usize))) {
            k.ws.set_onopen(None);
            k.ws.set_onmessage(None);
            k.ws.set_onclose(None);
            k.ws.set_onerror(None);
            let _ = k.ws.close();
        }
    }
}

#[no_mangle]
pub extern "C" fn link_state(l: *mut c_void) -> c_int {
    with(l, |k| k.state.get()).unwrap_or(CLOSED)
}

/// # Safety
/// `data` points at `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn link_send(l: *mut c_void, opcode: c_int, data: *const u8, len: usize) -> c_int {
    let bytes = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data, len) } };
    with(l, |k| {
        if k.state.get() != OPEN {
            return 0;
        }
        let sent = if opcode == 1 { k.ws.send_with_str(&String::from_utf8_lossy(bytes)) } else { k.ws.send_with_u8_array(bytes) };
        sent.is_ok() as c_int
    })
    .unwrap_or(0)
}

/// # Safety
/// `buf` has room for `cap` bytes, `len` is writable.
#[no_mangle]
pub unsafe extern "C" fn link_poll(l: *mut c_void, buf: *mut u8, cap: usize, len: *mut usize) -> c_int {
    with(l, |k| {
        let Some((op, data)) = k.inbox.borrow_mut().pop_front() else {
            return if k.state.get() == CLOSED { -1 } else { 0 };
        };
        if data.len() + 1 > cap {
            log("link: a frame too big for the buffer: closing");
            k.state.set(CLOSED);
            let _ = k.ws.close();
            return -1;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf, data.len());
            *buf.add(data.len()) = 0;
            *len = data.len();
        }
        op
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn link_close(l: *mut c_void) {
    with(l, |k| {
        k.state.set(CLOSED);
        k.inbox.borrow_mut().clear();
        let _ = k.ws.close_with_code(1000);
    });
}
