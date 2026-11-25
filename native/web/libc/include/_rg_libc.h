/* The browser build's C library (native/web/libc): only what the recompiled engine and its
 * runtime (native/rt) use. Files live in memory (fs.c); the heap and the transcendental math
 * come from the Rust host (native/web/src/libc.rs). */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include <stdarg.h>
