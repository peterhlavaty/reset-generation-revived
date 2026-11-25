/* Internal interfaces between the runtime pieces, and the services each platform host
 * (native/host/desktop, native/android) must provide. */
#pragma once
#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ------------------------------------------------------------------ provided by the platform */
void host_log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void host_fatal(const char *msg) __attribute__((noreturn));
uint32_t host_time_ms(void);              /* timeGetTime(): a millisecond clock */
void host_sleep_ms(uint32_t ms);          /* Sleep() */

/* ------------------------------------------------------------------ runtime internals */
#define RT_STUB_BASE 0xF0000000u          /* fake code addresses of host imports / JNI functions */

/* Guest address map. Native builds reserve all 4 GB (M + guest address). The browser build has
 * one linear memory: guest addresses are memory addresses there (M = 0; a base below them would
 * break, the compiler folds constant addresses into wasm load offsets, which do not wrap), and the
 * guest gets the image (0x10000000) and one block after it, above the runtime's own memory. The
 * oracle (native/oracle, a 32-bit process running the original DLL) uses the same compact map at
 * real addresses, and so does the recompiled build it is compared with (RT_COMPACT_MAP). */
#if defined(RT_ORACLE) && !defined(RT_COMPACT_MAP)
#define RT_COMPACT_MAP
#endif
#if defined(__wasm__) || defined(RT_COMPACT_MAP)
#define RT_GUEST_LOW 0x10000000u          /* the image's base */
#define RT_HEAP_BASE 0x10800000u          /* the image ends below this (checked when loading) */
#define RT_HEAP_SIZE 0x06000000u          /* 96 MB; a match uses about 20 */
#define RT_VIRT_BASE 0x16800000u
#define RT_VIRT_SIZE 0x01000000u
#define RT_STACK_BASE 0x17800000u
#define RT_TEB_ADDR 0x17B00000u
#define RT_JNI_BASE 0x17C00000u
#define RT_GUEST_END 0x17D00000u
#else
#define RT_STACK_BASE 0x00100000u
#define RT_TEB_ADDR 0x00400000u
#define RT_JNI_BASE 0x00500000u
#define RT_HEAP_BASE 0x20000000u
#define RT_HEAP_SIZE 0x20000000u
#define RT_VIRT_BASE 0x40000000u
#define RT_VIRT_SIZE 0x20000000u
#endif
#define RT_STACK_SIZE 0x00300000u
extern uint32_t rt_last_call;

void rt_fatal(const char *fmt, ...) __attribute__((noreturn, format(printf, 1, 2)));
void rt_mem_init(void);
void rt_dump_calltrace(void);
void rt_mem_commit(uint32_t addr, uint32_t size);
void rt_mem_report(void);   /* logs how much guest heap and VirtualAlloc space is in use */
void rt_dispatch_init(void);
uint32_t rt_guest_call(uint32_t addr, int nargs, const uint32_t *args);
uint32_t rt_guest_thiscall(uint32_t addr, uint32_t ecx, int nargs, const uint32_t *args);

/* Host stubs: guest-callable functions implemented in C. A stub reads its arguments with
 * rt_arg(i) and finishes with rt_ret(value, n) where n is the number of stack argument
 * slots the callee pops (stdcall) or 0 (cdecl). */
typedef void (*rt_stub_fn)(int arg);
uint32_t rt_stub_register(const char *name, rt_stub_fn fn, int arg);
void rt_stub_call(uint32_t addr);
const char *rt_stub_name(uint32_t addr);
static inline uint32_t rt_arg(int i);
void rt_ret(uint32_t eax, int pop_args);

/* 5x7 bitmap font (font.c): glyph rows (5 bits each), NULL for characters it does not have */
const uint8_t *rt_glyph(char c);
int rt_text_width(const char *s, int scale);
void rt_draw_text(uint32_t *px, int w, int h, int x, int y, int scale, const char *s, uint32_t color);
void rt_draw_text_centred(uint32_t *px, int w, int h, int y, int scale, const char *s, uint32_t color);
void rt_fill_rect(uint32_t *px, int w, int h, int x0, int y0, int x1, int y1, uint32_t color);
void rt_shade_rect(uint32_t *px, int w, int h, int x0, int y0, int x1, int y1, int keep);   /* keep/256 of the colour */
void rt_draw_panel(uint32_t *px, int w, int h, int x, int y, int bw, int bh);   /* the frame darkened, a gold-edged box */

/* guest heap (win32.c) */
uint32_t rt_heap_alloc(uint32_t n);
void rt_heap_free(uint32_t p);
uint32_t rt_heap_size(uint32_t p);
uint32_t rt_strdup_guest(const char *s);
const char *rt_guest_str(uint32_t p);     /* host view of a NUL-terminated guest string */

/* PE loader and Win32 layer (win32.c) */
typedef struct {
    uint32_t base, entry;
    const char *dir;                      /* host directory of the game data */
} rt_image;
int rt_load_image(rt_image *img, const char *dll_path, const char *game_dir);
uint32_t rt_export(const char *name_part);
void rt_set_virtual_time(int on);         /* deterministic clock for tests */
void rt_advance_time(uint32_t ms);
uint32_t rt_now_ms(void);

/* JNI layer (jni.c) */
void rt_jni_init(void);
uint32_t rt_jni_env(void);
uint32_t rt_jni_new_object(const char *kind);
uint32_t rt_jni_new_string(const char *s);
uint32_t rt_jni_new_direct_buffer(uint32_t guest_addr, uint32_t len);
const char *rt_jni_string(uint32_t handle);

/* Java methods the engine calls back (implemented by the engine glue in engine.c). */
typedef union { int32_t i; int64_t j; double d; uint32_t l; } rt_jvalue;
rt_jvalue rt_java_call(uint32_t obj, const char *name, const char *sig, const rt_jvalue *args, int nargs);

#include "rt.h"
static inline uint32_t rt_arg(int i) { return LD32(cpu.r[ESP] + 4 + 4 * (uint32_t)i); }

#ifdef __cplusplus
}
#endif
