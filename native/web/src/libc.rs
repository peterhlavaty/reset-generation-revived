//! What the C library of the browser build (native/web/libc) takes from Rust: the heap, the
//! transcendental math (libm), the console, the clocks, and keeping the files the engine writes.

use std::alloc::{alloc, alloc_zeroed, dealloc, realloc as re_alloc, Layout};
use std::ffi::{c_char, c_int, CStr};

// ------------------------------------------------------------------ heap
// Each block starts with its size (16 bytes, so the data stays 16-byte aligned like malloc's).
const HEAD: usize = 16;

fn layout(n: usize) -> Option<Layout> {
    Layout::from_size_align(n.checked_add(HEAD)?, HEAD).ok()
}

unsafe fn finish(p: *mut u8, n: usize) -> *mut u8 {
    if p.is_null() {
        return p;
    }
    (p as *mut usize).write(n);
    p.add(HEAD)
}

#[no_mangle]
pub unsafe extern "C" fn malloc(n: usize) -> *mut u8 {
    match layout(n) {
        Some(l) => finish(alloc(l), n),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    match n.checked_mul(size).and_then(|t| Some((t, layout(t)?))) {
        Some((t, l)) => finish(alloc_zeroed(l), t),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn free(p: *mut u8) {
    if !p.is_null() {
        let base = p.sub(HEAD);
        let n = (base as *const usize).read();
        dealloc(base, layout(n).unwrap());
    }
}

#[no_mangle]
pub unsafe extern "C" fn realloc(p: *mut u8, n: usize) -> *mut u8 {
    if p.is_null() {
        return malloc(n);
    }
    let Some(new) = layout(n) else { return std::ptr::null_mut() };
    let base = p.sub(HEAD);
    let old = (base as *const usize).read();
    finish(re_alloc(base, layout(old).unwrap(), new.size()), n)
}

// ------------------------------------------------------------------ math
macro_rules! math {
    ($($name:ident($($a:ident: $t:ty),*);)*) => {
        $(#[no_mangle] pub extern "C" fn $name($($a: $t),*) -> f64 { libm::$name($($a),*) })*
    };
}
math! {
    sin(x: f64); cos(x: f64); tan(x: f64); atan(x: f64); atan2(y: f64, x: f64);
    exp(x: f64); log(x: f64); pow(x: f64, y: f64); fmod(x: f64, y: f64); ldexp(x: f64, e: i32);
}

#[no_mangle]
pub unsafe extern "C" fn frexp(x: f64, e: *mut c_int) -> f64 {
    let (f, exp) = libm::frexp(x);
    *e = exp;
    f
}

// ------------------------------------------------------------------ console, clocks, files
#[no_mangle]
pub unsafe extern "C" fn rg_web_log(line: *const c_char, len: usize) {
    let s = String::from_utf8_lossy(std::slice::from_raw_parts(line as *const u8, len));
    web_sys::console::log_1(&s.as_ref().into());
}

#[no_mangle]
pub unsafe extern "C" fn rg_web_abort(why: *const c_char) -> ! {
    let msg = CStr::from_ptr(why).to_string_lossy().into_owned();
    crate::show_error(&format!("The game stopped: {msg}"));
    wasm_bindgen::throw_str(&msg)
}

#[no_mangle]
pub extern "C" fn rg_web_now_ms() -> f64 {
    crate::now_ms()
}

#[no_mangle]
pub extern "C" fn rg_web_epoch_ms() -> f64 {
    js_sys::Date::now()
}

#[no_mangle]
pub unsafe extern "C" fn rg_web_file_changed(path: *const c_char, data: *const u8, len: usize) {
    let path = CStr::from_ptr(path).to_string_lossy().into_owned();
    let data = (!data.is_null()).then(|| std::slice::from_raw_parts(data, len));
    crate::saves::store(&path, data);
}
