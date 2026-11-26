/* Android host for the recompiled Reset Generation engine (NativeActivity, no Java code).
 *
 * - Game data ships as APK assets and is copied to internal storage on the first run of each
 *   APK build (the engine does ordinary file I/O and writes its saves next to the data).
 * - One engine per process, driven by one activity at a time (Android may run two at once).
 * - The engine draws 640x360 (data pre-patched for that size at build time). The window buffer
 *   is 2x that height at the screen's aspect ratio; the game is scaled 2x into the middle and
 *   the touch controls live in the side bars, so they never cover the playfield.
 * - Input: on-screen d-pad + Action/Select/Back, hardware keyboards and gamepads, Back key.
 * - Audio: AAudio stream fed from the engine's 8 kHz mono mixer.
 * - Pausing (home, screen off) freezes the engine clock so its frame limiter does not try to
 *   catch up on resume. */
#include <aaudio/AAudio.h>
#include <android/asset_manager.h>
#include <android/log.h>
#include <android/native_activity.h>
#include <android/native_window.h>
#include <android/window.h>
#include <android_native_app_glue.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "engine.h"
#include "host.h"

#define TAG "ResetGeneration"
#define GW 640
#define GH 360
#define SCALE 2
#define BH (GH * SCALE)

/* Java AWT key codes the engine understands */
enum { K_ENTER = 10, K_CTRL = 17, K_ESC = 27, K_LEFT = 37, K_UP = 38, K_RIGHT = 39, K_DOWN = 40 };

/* ------------------------------------------------------------------ host services */
void host_log(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    __android_log_vprint(ANDROID_LOG_INFO, TAG, fmt, ap);
    va_end(ap);
}

void host_fatal(const char *msg) {
    __android_log_print(ANDROID_LOG_FATAL, TAG, "fatal: %s", msg);
    abort();
}

static uint64_t mono_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000 + (uint64_t)t.tv_nsec / 1000000;
}

static uint64_t paused_total, paused_at;
static int paused = 1;
uint32_t host_time_ms(void) { return (uint32_t)(mono_ms() - paused_total); }
void host_sleep_ms(uint32_t ms) { if (ms) usleep(ms * 1000); }

static void set_paused(int p) {
    if (p == paused) return;
    if (p) paused_at = mono_ms();
    else paused_total += mono_ms() - paused_at;
    paused = p;
}

/* ------------------------------------------------------------------ engine ownership */
/* NativeActivity runs one android_main thread per activity instance, and Android can start a
 * second instance while the first one's thread still runs (seen on the first launch after an
 * APK update: two threads installed the data and booted the engine at the same time, rg_init
 * crashed and the data files were left corrupted). The engine, the runtime and the host state
 * in this file exist once per process, so one activity at a time owns them. Every thread keeps
 * servicing its own looper (Android's UI thread waits for it on lifecycle calls) and holds the
 * engine lock while it handles events or runs a frame, but only the owner's events and frames
 * touch the engine and the host state. The engine boots once, by the first owner that has a
 * window; another activity takes it over when it gains focus (it is the one the user sees),
 * when it gets a window while the owner has none, or when the owner is destroyed. */
typedef struct activity {
    struct android_app *app;
    int focused;                        /* this activity's own focus */
    struct activity *next;
} activity;

/* The engine lock is a ticket lock (first come, first served): the owner's thread takes it
 * again right after each frame (which sleeps in the engine's frame limiter while holding it),
 * and a plain mutex would let it starve another activity's thread waiting to handle the events
 * the UI thread waits for. */
static pthread_mutex_t ticket_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t ticket_cv = PTHREAD_COND_INITIALIZER;
static unsigned ticket_next, ticket_now;

static void engine_lock(void) {
    pthread_mutex_lock(&ticket_mu);
    unsigned t = ticket_next++;
    while (t != ticket_now) pthread_cond_wait(&ticket_cv, &ticket_mu);
    pthread_mutex_unlock(&ticket_mu);
}

static void engine_unlock(void) {
    pthread_mutex_lock(&ticket_mu);
    ticket_now++;
    pthread_cond_broadcast(&ticket_cv);
    pthread_mutex_unlock(&ticket_mu);
}

static activity *acts;                  /* running activities, newest first */
static activity *owner;                 /* the one the engine draws to and takes input from */
static int focused;                     /* the owner has focus */
static int booted;                      /* rg_init done: nothing calls into the engine before */
static int booting;                     /* an owner is installing the data and booting */
static int quitting;                    /* the engine asked to quit (rg_tick returned 0) */

/* ------------------------------------------------------------------ assets -> internal storage */
/* Copies the APK's assets/game/ files to internal storage once per APK build. Existing saves
 * (settings.cfg, achievements.dat, *.save) are never overwritten.
 *
 * An interrupted install (the app killed or crashed, or two installs at once) must not leave
 * damaged files that pass for installed ones: every file is written under a temporary name,
 * checked against the asset's length, synced and renamed into place, and the stamp is removed
 * first and written last. The stamp names the data format and the APK build and lists every
 * installed file with its size; each start checks those sizes, so a damaged install is redone.
 * Format 1 (a bare build stamp) was written by installs that could run twice at once and
 * corrupt the files they shared, so its installs are redone once. */
#define DATA_FORMAT "rgdata 2"

static int is_save(const char *name) {
    return !strcmp(name, "settings.cfg") || !strcmp(name, "achievements.dat") || strstr(name, ".save") != NULL;
}

/* 1 if dir holds a complete install: the stamp starts with head (format and build), ends with
 * "end", and every file it lists still has the size it was installed with (quiet: the second
 * check, under the install lock, does not log the reason again) */
static int data_installed(const char *dir, const char *head, int quiet) {
    char path[1024], line[512], name[300];
    snprintf(path, sizeof path, "%s/.installed", dir);
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    int ok = fgets(line, sizeof line, f) && !strcmp(line, head), files = 0, end = 0;
    while (ok && fgets(line, sizeof line, f)) {
        long long size;
        struct stat st;
        if (!strcmp(line, "end\n")) { end = 1; break; }
        if (sscanf(line, "%lld %299s", &size, name) != 2) { ok = 0; break; }
        snprintf(path, sizeof path, "%s/%s", dir, name);
        if (stat(path, &st) != 0 || (long long)st.st_size != size) {
            if (!quiet) host_log("data: %s is missing or damaged, installing the game data again", name);
            ok = 0;
        }
        files++;
    }
    fclose(f);
    return ok && end && files > 0;
}

static int write_all(int fd, const char *p, size_t n) {
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return 0;
        p += w;
        n -= (size_t)w;
    }
    return 1;
}

/* makes the renames in dir durable (before the stamp that vouches for them) */
static void sync_dir(const char *dir) {
    int fd = open(dir, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd >= 0) { fsync(fd); close(fd); }
}

/* Copies asset game/<name> to dir/<name> through a temporary file. Returns its size, -1 on
 * failure (the old file, if any, is left as it was). */
static long long copy_asset(AAssetManager *am, const char *dir, const char *name) {
    char asset[300], path[1024], tmp[1024];
    char buf[65536];                     /* per call: a shared buffer corrupted concurrent copies */
    snprintf(asset, sizeof asset, "game/%s", name);
    snprintf(path, sizeof path, "%s/%s", dir, name);
    snprintf(tmp, sizeof tmp, "%s/.%s.part", dir, name);   /* no ".pak" suffix: the engine lists *.pak */
    AAsset *a = AAssetManager_open(am, asset, AASSET_MODE_STREAMING);
    if (!a) { host_log("data: asset %s missing", asset); return -1; }
    long long want = (long long)AAsset_getLength64(a), done = 0;
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644), ok = fd >= 0, k = 0;
    while (ok && (k = AAsset_read(a, buf, sizeof buf)) > 0) {
        ok = write_all(fd, buf, (size_t)k);
        done += k;
    }
    AAsset_close(a);
    if (k < 0) ok = 0;
    if (fd >= 0) {
        if (fsync(fd) != 0) ok = 0;
        if (close(fd) != 0) ok = 0;
    }
    struct stat st;
    if (ok && (done != want || stat(tmp, &st) != 0 || (long long)st.st_size != want)) ok = 0;
    if (ok && rename(tmp, path) != 0) ok = 0;
    if (!ok) {
        host_log("data: could not install %s (%lld of %lld bytes, errno %d)", name, done, want, errno);
        unlink(tmp);
        return -1;
    }
    return want;
}

static int install_files(AAssetManager *am, const char *dir, const char *head) {
    char stamp[1024], tmp[1024], path[1024];
    snprintf(stamp, sizeof stamp, "%s/.installed", dir);
    snprintf(tmp, sizeof tmp, "%s/.installed.part", dir);
    unlink(stamp);                       /* no stamp until every file is in place */
    host_log("data: installing %.*s", (int)strcspn(head, "\n"), head);
    AAssetDir *ad = AAssetManager_openDir(am, "game");
    FILE *list = ad ? fopen(tmp, "w") : NULL;
    if (!list) { if (ad) AAssetDir_close(ad); return 0; }
    fputs(head, list);
    const char *name;
    int n = 0, ok = 1;
    while (ok && (name = AAssetDir_getNextFileName(ad))) {
        struct stat st;
        snprintf(path, sizeof path, "%s/%s", dir, name);
        if (is_save(name)) {             /* the player's own once it exists; not listed (the engine rewrites it) */
            if (stat(path, &st) != 0 && copy_asset(am, dir, name) < 0) ok = 0;
            continue;
        }
        long long size = copy_asset(am, dir, name);
        if (size < 0) { ok = 0; break; }
        fprintf(list, "%lld %s\n", size, name);
        n++;
    }
    AAssetDir_close(ad);
    fputs("end\n", list);
    if (fflush(list) != 0 || fsync(fileno(list)) != 0) ok = 0;
    if (fclose(list) != 0) ok = 0;
    if (ok && n > 0) {
        sync_dir(dir);
        if (rename(tmp, stamp) == 0) {
            sync_dir(dir);
            host_log("installed %d data files into %s", n, dir);
            return 1;
        }
    }
    unlink(tmp);
    return 0;
}

static int install_data(AAssetManager *am, const char *dir, const char *build) {
    char head[160], path[1024];
    snprintf(head, sizeof head, "%s %s\n", DATA_FORMAT, build);
    mkdir(dir, 0755);
    if (data_installed(dir, head, 0)) return 1;
    /* exclusive also against another process of the app (an old one still shutting down) */
    snprintf(path, sizeof path, "%s/.install.lock", dir);
    int lock = open(path, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    if (lock >= 0) while (flock(lock, LOCK_EX) != 0 && errno == EINTR) {}
    int ok = data_installed(dir, head, 1) || install_files(am, dir, head);
    if (lock >= 0) { flock(lock, LOCK_UN); close(lock); }
    return ok;
}

/* ------------------------------------------------------------------ audio */
#define RING 16384                      /* samples (2 s at 8 kHz) */
static int16_t ring[RING];
static volatile uint32_t ring_r, ring_w;
static AAudioStream *stream;
static int out_rate = 48000;
static double resample_pos;

static aaudio_data_callback_result_t audio_cb(AAudioStream *s, void *user, void *data, int32_t frames) {
    (void)s; (void)user;
    int16_t *out = (int16_t *)data;
    double step = 8000.0 / out_rate;
    for (int32_t i = 0; i < frames; i++) {
        uint32_t avail = ring_w - ring_r;
        int16_t v = 0;
        if (avail > 1) {
            int16_t a = ring[ring_r % RING], b = ring[(ring_r + 1) % RING];
            v = (int16_t)(a + (b - a) * resample_pos);       /* linear interpolation */
            resample_pos += step;
            while (resample_pos >= 1.0) { resample_pos -= 1.0; ring_r++; }
        }
        out[i] = v;
    }
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

static void audio_start(void) {
    AAudioStreamBuilder *b;
    if (stream) return;                 /* already playing (focus and a takeover both start it) */
    if (AAudio_createStreamBuilder(&b) != AAUDIO_OK) return;
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setChannelCount(b, 1);
    AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    AAudioStreamBuilder_setDataCallback(b, audio_cb, NULL);
    if (AAudioStreamBuilder_openStream(b, &stream) == AAUDIO_OK) {
        out_rate = AAudioStream_getSampleRate(stream);
        AAudioStream_requestStart(stream);
        host_log("audio: %d Hz", out_rate);
    }
    AAudioStreamBuilder_delete(b);
}

static void audio_stop(void) {
    if (stream) { AAudioStream_requestStop(stream); AAudioStream_close(stream); stream = NULL; }
}

/* keep ~100 ms of engine audio queued */
static void audio_pump(void) {
    static uint8_t buf[2048];
    while (ring_w - ring_r < 800) {
        rg_audio(buf, 512);
        for (int i = 0; i < 256; i++) { ring[ring_w % RING] = ((int16_t *)buf)[i]; ring_w++; }
    }
}

/* ------------------------------------------------------------------ touch controls */
typedef struct { int key; float cx, cy, r; const char *label; int down, sent; } button;   /* sent: the engine got the press */
static button buttons[] = {
    {K_UP, 0, 0, 0, "up", 0}, {K_DOWN, 0, 0, 0, "down", 0}, {K_LEFT, 0, 0, 0, "left", 0}, {K_RIGHT, 0, 0, 0, "right", 0},
    {K_CTRL, 0, 0, 0, "action", 0}, {K_ENTER, 0, 0, 0, "select", 0}, {K_ESC, 0, 0, 0, "back", 0},
    {K_CTRL, 0, 0, 0, "drop", 0}, {0, 0, 0, 0, "host", 0},
};
#define B_DROP 7
#define B_HOST 8               /* host a game: shown only where F5 works (host_button_on) */

int rg_host_can_start(void);   /* server.c: 1 = the main menu is up and nothing is being hosted */

/* shown and active only where hosting can start (like F5): elsewhere the request is refused,
 * and the button would only cover the screen */
static int host_button_on(void) { return booted && rg_host_can_start(); }

/* The game drops a piece when Action is held for 500 ms. The Drop button is a tap that holds
 * Ctrl for DROP_MS by itself, so no finger has to stay down. */
#define DROP_MS 700
static int drop_pending;
static uint64_t drop_release_at;
#define NB (int)(sizeof buttons / sizeof buttons[0])
static int bw;                           /* window buffer width (height is BH) */
static int game_x;                       /* left edge of the game image in the buffer */
static int controls_visible = 1;
static uint64_t last_pad_input;
static int pointer_button[16];           /* which button each touch pointer holds (-1 none) */

#ifdef RG_DIAG
/* Diagnostics build: an on-screen overlay (top-left; tap it to hide/show) and logcat lines
 * showing what input the host receives and how fast the engine runs. For remote debugging
 * on devices we cannot touch: the user sends a screenshot or screen recording. */
static struct {
    int show;
    unsigned events, hovers, cancels;
    int tool, act, last_btn;                 /* last pointer 0 event */
    float x, y;
    uint64_t down_at[NB];                    /* per button (8 overflowed on the HOST button) */
    unsigned taps[NB], hold_last[NB], hold_max[NB];
    unsigned ticks_s, frames_s, tick_max_s;  /* last full second */
    unsigned ticks, frames, tick_max;        /* current second */
    uint64_t sec_at;
} dg = { .show = 1 };
#endif

static void layout_controls(void) {
    float side = (float)game_x;               /* width of each side bar */
    float u = BH / 10.0f;
    float lx = side > 3 * u ? side / 2 : 2.2f * u, ly = BH * 0.62f;
    float d = 1.25f * u, r = 0.85f * u;
    buttons[0] = (button){K_UP, lx, ly - d, r, "up", 0};
    buttons[1] = (button){K_DOWN, lx, ly + d, r, "down", 0};
    buttons[2] = (button){K_LEFT, lx - d, ly, r, "left", 0};
    buttons[3] = (button){K_RIGHT, lx + d, ly, r, "right", 0};
    float rx = bw - (side > 3 * u ? side / 2 : 2.2f * u);
    buttons[4] = (button){K_CTRL, rx, ly, 1.5f * u, "action", 0};
    buttons[5] = (button){K_ENTER, lx, 1.1f * u, 0.7f * u, "select", 0};
    buttons[6] = (button){K_ESC, rx, 1.1f * u, 0.7f * u, "back", 0};
    buttons[B_DROP] = (button){K_CTRL, rx, ly - 3.0f * u, 0.95f * u, "drop", 0};
    buttons[B_HOST] = (button){0, lx, 3.0f * u, 0.62f * u, "host", 0};
}

static void drop_release(void) {
    if (!drop_pending) return;
    drop_pending = 0;
    rg_key(0, K_CTRL);
}

static void press(int b, int down) {
    if (buttons[b].down == down) return;
    buttons[b].down = down;
#ifdef RG_DIAG
    if (down) { dg.down_at[b] = mono_ms(); dg.taps[b]++; }
    else if (dg.down_at[b]) {
        dg.hold_last[b] = (unsigned)(mono_ms() - dg.down_at[b]);
        if (dg.hold_last[b] > dg.hold_max[b]) dg.hold_max[b] = dg.hold_last[b];
        host_log("diag: button %s released after %u ms", buttons[b].label, dg.hold_last[b]);
        dg.down_at[b] = 0;
    }
#endif
    if (!booted) return;                   /* a touch before the engine is up only lights the button */
    if (b == B_HOST) {
        if (down && host_button_on()) rg_host_request();
        return;
    }
    if (b == B_DROP) {                     /* released by the main loop after DROP_MS */
        if (down) {
            if (!drop_pending) rg_key(1, K_CTRL);
            drop_pending = 1;
            drop_release_at = mono_ms() + DROP_MS;
        }
        return;
    }
    if (!down && !buttons[b].sent) return;  /* pressed before the engine was up: no release either */
    buttons[b].sent = down;
    rg_key(down, buttons[b].key);
}

static int hit(float x, float y) {
    int best = -1;
    float bestd = 1e9f;
    int host_on = host_button_on();
    for (int i = 0; i < NB; i++) {
        if (i == B_HOST && !host_on) continue;
        float dx = x - buttons[i].cx, dy = y - buttons[i].cy, dd = dx * dx + dy * dy;
        float reach = buttons[i].r * 1.35f;
        if (dd < reach * reach && dd < bestd) { best = i; bestd = dd; }
    }
    return best;
}

/* Soft-key labels drawn by the engine in the bottom corners of the game image act as
 * Select / Back when tapped. */
static int hit_softkeys(float x, float y) {
    if (y < BH - BH / 9) return -1;
    if (x >= game_x && x < game_x + 160) return 5;
    if (x >= game_x + GW * SCALE - 160 && x < game_x + GW * SCALE) return 6;
    return -1;
}

/* The controls are white shapes at various opacities, so they are rendered once into an
 * alpha mask (rebuilt only when a button changes state) and composited cheaply each frame. */
static uint8_t *mask;
static int mask_y0[2048], mask_y1[2048];  /* per row: x range containing coverage */
static unsigned mask_key = ~0u;           /* state the mask was built for */
static int dirty = 1;                     /* window content must be redrawn */

static void cover(int x, int y, int a) {
    if (x < 0 || x >= bw || y < 0 || y >= BH) return;
    uint8_t *m = &mask[y * bw + x];
    *m = (uint8_t)(a + *m * (255 - a) / 255);
    if (x < mask_y0[y]) mask_y0[y] = x;
    if (x > mask_y1[y]) mask_y1[y] = x;
}

static void mask_circle(float cx, float cy, float r, int alpha, int ring_only) {
    int x0 = (int)(cx - r - 1), x1 = (int)(cx + r + 1), y0 = (int)(cy - r - 1), y1 = (int)(cy + r + 1);
    for (int y = y0; y <= y1; y++)
        for (int x = x0; x <= x1; x++) {
            float d2 = (x - cx) * (x - cx) + (y - cy) * (y - cy);
            if (d2 > r * r) continue;
            if (ring_only && d2 < (r - 4) * (r - 4)) continue;
            cover(x, y, alpha);
        }
}

static void mask_triangle(float cx, float cy, float s, int dir, int alpha) {
    /* dir: 0 up 1 down 2 left 3 right */
    for (int y = (int)(cy - s); y <= (int)(cy + s); y++)
        for (int x = (int)(cx - s); x <= (int)(cx + s); x++) {
            float u = (x - cx) / s, v = (y - cy) / s;
            float along = dir == 0 ? -v : dir == 1 ? v : dir == 2 ? -u : u;
            float across = (dir < 2) ? u : v;
            if (along < -0.5f || along > 0.6f) continue;
            if (fabsf(across) > (0.6f - along) * 0.9f) continue;
            cover(x, y, alpha);
        }
}

static int text_w(const char *s, int sc) { return (int)strlen(s) * 6 * sc - sc; }

/* centred at cx, top at y */
static void mask_text(float cx, int y, int sc, const char *s, int alpha) {
    int x = (int)cx - text_w(s, sc) / 2;
    for (; *s; s++, x += 6 * sc) {
        const uint8_t *g = rt_glyph(*s);
        if (!g) continue;
        for (int r = 0; r < 7; r++)
            for (int c = 0; c < 5; c++)
                if (g[r] >> (4 - c) & 1)
                    for (int dy = 0; dy < sc; dy++)
                        for (int dx = 0; dx < sc; dx++) cover(x + c * sc + dx, y + r * sc + dy, alpha);
    }
}

static void build_mask(void) {
    unsigned key = (unsigned)controls_visible | (unsigned)bw << 1;
    int host_on = host_button_on();
    for (int i = 0; i < NB; i++) key = key * 31 + (unsigned)buttons[i].down;
    key = key * 31 + (unsigned)host_on;
    if (key == mask_key && mask) return;
    mask_key = key;
    dirty = 1;
    free(mask);
    mask = (uint8_t *)calloc((size_t)bw * BH, 1);
    for (int y = 0; y < BH; y++) { mask_y0[y] = bw; mask_y1[y] = -1; }
    if (!controls_visible) return;
    for (int i = 0; i < NB; i++) {
        if (i == B_HOST && !host_on) continue;
        button *b = &buttons[i];
        mask_circle(b->cx, b->cy, b->r, b->down ? 150 : 70, 0);
        mask_circle(b->cx, b->cy, b->r, 140, 1);
        if (i < 4) mask_triangle(b->cx, b->cy, b->r * 0.55f, i, 200);
        else if (i == 4) {
            mask_circle(b->cx, b->cy, b->r * 0.35f, 170, 0);
            mask_text(b->cx, (int)(b->cy + b->r) + 14, 4, "ROTATE / OK", 230);
        } else if (i == B_HOST) {
            mask_text(b->cx, (int)b->cy - 10, 3, "HOST", 230);
        } else if (i == B_DROP) {
            mask_text(b->cx, (int)(b->cy + b->r) + 10, 4, "DROP", 230);
            /* a down arrow onto a bar: "place" */
            mask_triangle(b->cx, b->cy - b->r * 0.1f, b->r * 0.5f, 1, 200);
            for (int k = -(int)(b->r * 0.45f); k <= (int)(b->r * 0.45f); k++)
                for (int t = 0; t < 5; t++) cover((int)(b->cx + k), (int)(b->cy + b->r * 0.5f + t), 200);
        } else {
            /* select: horizontal bar; back: "x" */
            for (int k = -(int)(b->r * 0.4f); k <= (int)(b->r * 0.4f); k++)
                for (int t = -2; t <= 2; t++) {
                    if (i == 5) cover((int)(b->cx + k), (int)(b->cy + t), 200);
                    else {
                        cover((int)(b->cx + k), (int)(b->cy + k + t), 200);
                        cover((int)(b->cx + k), (int)(b->cy - k + t), 200);
                    }
                }
        }
    }
}

static void composite_controls(uint32_t *px, int stride) {
    for (int y = 0; y < BH; y++) {
        const uint8_t *m = mask + (size_t)y * bw;
        uint32_t *row = px + (size_t)y * stride;
        for (int x = mask_y0[y]; x <= mask_y1[y]; x++) {
            unsigned a = m[x];
            if (!a) continue;
            uint32_t d = row[x];
            uint32_t r = ((d & 0xff) * (255 - a) + 255 * a) / 255;
            uint32_t g = (((d >> 8) & 0xff) * (255 - a) + 255 * a) / 255;
            uint32_t b = (((d >> 16) & 0xff) * (255 - a) + 255 * a) / 255;
            row[x] = 0xff000000u | b << 16 | g << 8 | r;
        }
    }
}

#ifdef RG_DIAG
#define DG_LINES 8
#define DG_SC 2
static int dg_x(void) { return game_x + 8; }
static int dg_w(void) { return 36 * 6 * DG_SC; }
static int dg_h(void) { return DG_LINES * 9 * DG_SC + 6; }

static void dg_text(uint32_t *px, int stride, int x, int y, const char *s) {
    for (; *s; s++, x += 6 * DG_SC) {
        const uint8_t *g = rt_glyph(*s);
        if (!g) continue;
        for (int r = 0; r < 7; r++)
            for (int c = 0; c < 5; c++)
                if (g[r] >> (4 - c) & 1)
                    for (int dy = 0; dy < DG_SC; dy++)
                        for (int dx = 0; dx < DG_SC; dx++) px[(size_t)(y + r * DG_SC + dy) * stride + x + c * DG_SC + dx] = 0xffffffffu;
    }
}

static void diag_draw(uint32_t *px, int stride) {
    if (!dg.show) return;
    int x0 = dg_x(), y0 = 8;
    for (int y = y0; y < y0 + dg_h() && y < BH; y++)
        for (int x = x0 - 4; x < x0 + dg_w() && x < bw; x++) px[(size_t)y * stride + x] = 0xff000000u;
    char l[64];
    int y = y0 + 3;
    snprintf(l, sizeof l, "BUF %dX%d GX %d", bw, BH, game_x); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "TICKS %u/S FRAMES %u/S MAX %uMS", dg.ticks_s, dg.frames_s, dg.tick_max_s); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "EV %u ACT %d TOOL %d BTN %d", dg.events, dg.act, dg.tool, dg.last_btn); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "XY %d,%d", (int)dg.x, (int)dg.y); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "HOVER %u CANCEL %u", dg.hovers, dg.cancels); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "ACTION N %u LAST %uMS MAX %uMS", dg.taps[4], dg.hold_last[4], dg.hold_max[4]); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "DROP N %u LAST %uMS MAX %uMS", dg.taps[B_DROP], dg.hold_last[B_DROP], dg.hold_max[B_DROP]); dg_text(px, stride, x0, y, l); y += 9 * DG_SC;
    snprintf(l, sizeof l, "TOOL 1 FINGER 2 PEN 3 MOUSE"); dg_text(px, stride, x0, y, l);
}

static void diag_second(uint64_t now) {
    if (now - dg.sec_at < 1000) return;
    dg.sec_at = now;
    dg.ticks_s = dg.ticks; dg.frames_s = dg.frames; dg.tick_max_s = dg.tick_max;
    dg.ticks = dg.frames = dg.tick_max = 0;
    dirty = 1;
}
#endif

/* ------------------------------------------------------------------ rendering */
static ANativeWindow *window;

static void setup_window(void) {
    int ww = ANativeWindow_getWidth(window), wh = ANativeWindow_getHeight(window);
    if (ww < wh) { int t = ww; ww = wh; wh = t; }
    bw = (int)((long)BH * ww / wh);
    if (bw < GW * SCALE) bw = GW * SCALE;
    bw &= ~1;
    game_x = (bw - GW * SCALE) / 2;
    ANativeWindow_setBuffersGeometry(window, bw, BH, WINDOW_FORMAT_RGBX_8888);
    layout_controls();
    mask_key = ~0u;
    dirty = 1;
    host_log("window %dx%d -> buffer %dx%d", ww, wh, bw, BH);
}

static uint32_t last_frame[GW * GH];

static void present(void) {
    const uint32_t *src = rg_pixels();
    build_mask();
    static uint64_t forced_at;                /* repaint at least once a second, whatever changed */
    if (mono_ms() - forced_at > 1000) { forced_at = mono_ms(); dirty = 1; }
    if (memcmp(last_frame, src, sizeof last_frame) != 0) {
        memcpy(last_frame, src, sizeof last_frame);
        dirty = 1;
#ifdef RG_DIAG
        dg.frames++;
#endif
    }
    if (!dirty || !window) return;
    ANativeWindow_Buffer buf;
    if (ANativeWindow_lock(window, &buf, NULL) != 0) return;
    if (buf.width != bw || buf.height != BH) { ANativeWindow_unlockAndPost(window); setup_window(); return; }
    uint32_t *px = (uint32_t *)buf.bits;
    for (int y = 0; y < BH; y++) {
        uint32_t *row = px + (size_t)y * buf.stride;
        const uint32_t *s = last_frame + (y / SCALE) * GW;
        uint32_t *d = row + game_x;
        for (int x = 0; x < GW; x++) {
            uint32_t c = s[x];
            uint32_t v = 0xff000000u | (c & 0x00ff00u) | (c >> 16 & 0xff) | (c & 0xff) << 16;
            d[2 * x] = v;
            d[2 * x + 1] = v;
        }
        /* side bars: extend each row's edge pixel outward so the scene's background fills the screen */
        for (int x = 0; x < game_x; x++) row[x] = d[0];
        for (int x = game_x + GW * SCALE; x < bw; x++) row[x] = d[GW * SCALE - 1];
    }
    composite_controls(px, buf.stride);
#ifdef RG_DIAG
    diag_draw(px, buf.stride);
#endif
    ANativeWindow_unlockAndPost(window);
    /* Android may hand back an older buffer next time (double buffering): draw two frames */
    static int again;
    dirty = again = !again;
}

/* ------------------------------------------------------------------ input */
static int map_key(int32_t code) {
    switch (code) {
    case AKEYCODE_DPAD_UP: case AKEYCODE_W: return K_UP;
    case AKEYCODE_DPAD_DOWN: case AKEYCODE_S: return K_DOWN;
    case AKEYCODE_DPAD_LEFT: case AKEYCODE_A: return K_LEFT;
    case AKEYCODE_DPAD_RIGHT: case AKEYCODE_D: return K_RIGHT;
    case AKEYCODE_BUTTON_A: case AKEYCODE_DPAD_CENTER: case AKEYCODE_CTRL_LEFT: case AKEYCODE_CTRL_RIGHT:
    case AKEYCODE_SPACE: case AKEYCODE_BUTTON_R1: return K_CTRL;
    case AKEYCODE_BUTTON_START: case AKEYCODE_ENTER: case AKEYCODE_BUTTON_X: case AKEYCODE_BUTTON_L1: return K_ENTER;
    case AKEYCODE_BUTTON_B: case AKEYCODE_BACK: case AKEYCODE_ESCAPE: case AKEYCODE_BUTTON_SELECT: return K_ESC;
    default: return 0;
    }
}

/* engine lock held (see android_main) */
static int32_t on_input(struct android_app *app, AInputEvent *ev) {
    if ((activity *)app->userData != owner) return 0;   /* another activity drives the engine */
    int type = AInputEvent_getType(ev);
    if (type == AINPUT_EVENT_TYPE_KEY) {
        int k = map_key(AKeyEvent_getKeyCode(ev));
        if (!k) return 0;
        int action = AKeyEvent_getAction(ev);
        if (AKeyEvent_getRepeatCount(ev) > 0) return 1;
        if (booted && (action == AKEY_EVENT_ACTION_DOWN || action == AKEY_EVENT_ACTION_UP))
            rg_key(action == AKEY_EVENT_ACTION_DOWN, k);
        if (AKeyEvent_getKeyCode(ev) != AKEYCODE_BACK) { controls_visible = 0; last_pad_input = mono_ms(); }
        return 1;
    }
    if (type == AINPUT_EVENT_TYPE_MOTION) {
        int src = AInputEvent_getSource(ev);
        if (src & AINPUT_SOURCE_JOYSTICK) {     /* analog stick / hat as d-pad */
            float hx = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_HAT_X, 0);
            float hy = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_HAT_Y, 0);
            float sx = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_X, 0);
            float sy = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_Y, 0);
            float x = fabsf(hx) > fabsf(sx) ? hx : sx, y = fabsf(hy) > fabsf(sy) ? hy : sy;
            press(2, x < -0.5f); press(3, x > 0.5f); press(0, y < -0.5f); press(1, y > 0.5f);
            controls_visible = 0;
            last_pad_input = mono_ms();
            return 1;
        }
        if (!window) return 0;
        controls_visible = 1;
        int action = AMotionEvent_getAction(ev);
        int masked = action & AMOTION_EVENT_ACTION_MASK;
        int idx = (action & AMOTION_EVENT_ACTION_POINTER_INDEX_MASK) >> AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT;
        int ww = ANativeWindow_getWidth(window), wh = ANativeWindow_getHeight(window);
        size_t n = AMotionEvent_getPointerCount(ev);
#ifdef RG_DIAG
        {
            float dx = AMotionEvent_getX(ev, 0) * bw / ww, dy = AMotionEvent_getY(ev, 0) * BH / wh;
            dg.events++;
            dg.act = masked;
            dg.tool = AMotionEvent_getToolType(ev, 0);
            dg.x = dx; dg.y = dy;
            if (masked == AMOTION_EVENT_ACTION_HOVER_ENTER || masked == AMOTION_EVENT_ACTION_HOVER_MOVE ||
                masked == AMOTION_EVENT_ACTION_HOVER_EXIT) dg.hovers++;
            if (masked == AMOTION_EVENT_ACTION_CANCEL) dg.cancels++;
            if (masked == AMOTION_EVENT_ACTION_DOWN && dg.show && dx >= dg_x() && dx < dg_x() + dg_w() && dy < 8 + dg_h() && hit(dx, dy) < 0) {
                dg.show = 0;                       /* tap the overlay to hide it; tap its corner to bring it back */
            } else if (masked == AMOTION_EVENT_ACTION_DOWN && !dg.show && dx < game_x + 120 && dy < 120 && hit(dx, dy) < 0) {
                dg.show = 1;
            }
            if (masked != AMOTION_EVENT_ACTION_MOVE && masked != AMOTION_EVENT_ACTION_HOVER_MOVE)
                host_log("diag: act=%d tool=%d x=%d y=%d btn=%d n=%d", masked, dg.tool, (int)dx, (int)dy, hit(dx, dy), (int)n);
            dirty = 1;
        }
#endif
        for (size_t i = 0; i < n; i++) {
            int id = AMotionEvent_getPointerId(ev, i) & 15;
            float x = AMotionEvent_getX(ev, i) * bw / ww, y = AMotionEvent_getY(ev, i) * BH / wh;
            int up = (masked == AMOTION_EVENT_ACTION_UP || masked == AMOTION_EVENT_ACTION_CANCEL) ||
                     (masked == AMOTION_EVENT_ACTION_POINTER_UP && (int)i == idx);
            int b = up ? -1 : hit(x, y);
            if (b < 0 && !up && (masked == AMOTION_EVENT_ACTION_DOWN || masked == AMOTION_EVENT_ACTION_POINTER_DOWN) && (int)i == idx)
                b = hit_softkeys(x, y);
            if (b < 0 && !up && pointer_button[id] >= 4) b = pointer_button[id];   /* keep holding Action etc. */
            if (pointer_button[id] != b) {
                if (pointer_button[id] >= 0) press(pointer_button[id], 0);
                pointer_button[id] = b;
                if (b >= 0) press(b, 1);
            }
        }
#ifdef RG_DIAG
        dg.last_btn = pointer_button[AMotionEvent_getPointerId(ev, 0) & 15];
#endif
        return 1;
    }
    return 0;
}

/* ------------------------------------------------------------------ lifecycle */
/* (which activity drives the engine: see "engine ownership" above) */
static char data_dir[512];

/* engine lock held: the owner stops driving the engine; what it holds down is released */
static void let_go(void) {
    focused = 0;
    set_paused(1);
    audio_stop();
    for (int i = 0; i < NB; i++) if (buttons[i].down) press(i, 0);
    drop_release();
    for (int i = 0; i < 16; i++) pointer_button[i] = -1;
    window = NULL;
}

/* engine lock held: the engine moves to activity a (its window, focus and input) */
static void adopt(activity *a) {
    if (owner == a) return;
    if (owner) {
        host_log("engine: moves from activity %p to %p", (void *)owner->app->activity, (void *)a->app->activity);
        let_go();
    }
    owner = a;
    window = a->app->window;
    focused = a->focused;
    if (window) setup_window();
    if (booted && focused) { audio_start(); set_paused(0); }
    ALooper_wake(a->app->looper);        /* its thread may be waiting for events, not running frames */
}

/* engine lock held: activity a is being destroyed (its android_main returns after this) */
static void leave(activity *a) {
    for (activity **p = &acts; *p; p = &(*p)->next)
        if (*p == a) { *p = a->next; break; }
    if (owner != a) return;
    if (!acts || quitting) {
        /* The engine and runtime keep their state in process-wide globals and cannot be
         * re-initialised, so a process that outlives its last activity would relaunch into the
         * dead engine. Take the process down with the activity (exit flushes saves). */
        audio_stop();
        exit(0);
    }
    adopt(acts);                         /* the newest activity still running */
}

/* engine lock held (see android_main) */
static void on_cmd(struct android_app *app, int32_t cmd) {
    activity *a = (activity *)app->userData;
    if (cmd == APP_CMD_GAINED_FOCUS) a->focused = 1;
    if (cmd == APP_CMD_LOST_FOCUS || cmd == APP_CMD_PAUSE) a->focused = 0;
    if (a != owner && (cmd == APP_CMD_GAINED_FOCUS || (cmd == APP_CMD_INIT_WINDOW && !window))) adopt(a);
    if (a != owner) return;              /* the engine is another activity's */
    switch (cmd) {
    case APP_CMD_INIT_WINDOW:
        window = app->window;
        setup_window();
        break;
    case APP_CMD_TERM_WINDOW:
        window = NULL;
        break;
    case APP_CMD_GAINED_FOCUS:
        focused = 1;
        if (booted) { audio_start(); set_paused(0); }
        break;
    case APP_CMD_LOST_FOCUS:
    case APP_CMD_PAUSE:
        focused = 0;
        set_paused(1);
        audio_stop();
        for (int i = 0; i < NB; i++) if (buttons[i].down) press(i, 0);
        drop_release();
        break;
    case APP_CMD_WINDOW_RESIZED:
    case APP_CMD_CONFIG_CHANGED:
        if (window) setup_window();
        break;
    }
}

static char cfg_server[200] = "auto", cfg_nick[32];
static int cfg_players = 2;

static void save_config(void) {
    char path[600];
    snprintf(path, sizeof path, "%s/../revived.ini", data_dir);
    FILE *f = fopen(path, "w");
    if (f) { fprintf(f, "server=%s\nnickname=%s\nhost_players=%d\n", cfg_server, cfg_nick, cfg_players); fclose(f); }
}

static void read_config(void) {
    /* files/../revived.ini: server=auto|online|lan|ws://host:port/ws, nickname=Name, host_players=2..4 */
    char path[600], line[256], *server = cfg_server, *nick = cfg_nick;
    snprintf(path, sizeof path, "%s/../revived.ini", data_dir);
    FILE *f = fopen(path, "r");
    while (f && fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        if (!strncmp(line, "server=", 7)) snprintf(server, sizeof cfg_server, "%s", line + 7);
        if (!strncmp(line, "nickname=", 9)) snprintf(nick, sizeof cfg_nick, "%s", line + 9);
        if (!strncmp(line, "host_players=", 13)) cfg_players = atoi(line + 13);
    }
    if (f) fclose(f);
    if (!nick[0]) {
        srand((unsigned)mono_ms());
        snprintf(nick, sizeof cfg_nick, "Droid%04d", rand() % 9000 + 1000);
        save_config();
    }
    rg_net_configure(server, nick);
    rg_host_configure(cfg_players, -2);
    host_log("online: %s as %s", server, nick);
}

/* Installs the data and boots the engine. Runs on the owner's thread without the engine lock,
 * since it takes a while and the other activities' threads must keep answering the UI thread
 * meanwhile; `booting` keeps it the only boot in the process, and nothing calls into the engine
 * until `booted` is set. */
static void boot(struct android_app *app) {
    if (!install_data(app->activity->assetManager, data_dir, RG_DATA_STAMP))
        host_fatal("could not install the game data (APK assets missing, or no storage space)");
    read_config();
    char dll[600];
    snprintf(dll, sizeof dll, "%s/ResetGeneration-Win32-Web.dll", data_dir);
    if (!rg_init(dll, data_dir, GW, GH, "en")) host_fatal("engine failed to start");
}

/* engine lock held: a's thread runs frames (or the boot) instead of waiting for events */
static int wants_frames(activity *a) {
    return owner == a && window && (booted ? focused : !booting);
}

void android_main(struct android_app *app) {
    activity self = {app, 0, NULL};
    app->userData = &self;
    app->onAppCmd = on_cmd;
    app->onInputEvent = on_input;
    ANativeActivity_setWindowFlags(app->activity, AWINDOW_FLAG_KEEP_SCREEN_ON | AWINDOW_FLAG_FULLSCREEN, 0);
    engine_lock();
    self.next = acts;
    acts = &self;
    if (!owner) {                        /* the process's first activity */
        for (int i = 0; i < 16; i++) pointer_button[i] = -1;
        paused_at = mono_ms();           /* clock starts paused until the window has focus */
        snprintf(data_dir, sizeof data_dir, "%s/game", app->activity->internalDataPath);
        owner = &self;
    } else {
        host_log("engine: activity %p started while %p drives the engine (%s)", (void *)app->activity,
                 (void *)owner->app->activity, booted ? "running" : booting ? "booting" : "not booted");
    }
    engine_unlock();

    int busy = 0;
    for (;;) {
        int events;
        struct android_poll_source *src;
        while (ALooper_pollOnce(busy ? 0 : -1, NULL, &events, (void **)&src) >= 0) {
            engine_lock();
            if (src) src->process(app, src);
            if (app->destroyRequested) {
                leave(&self);            /* exits the process if this was its last activity */
                engine_unlock();
                return;
            }
            busy = wants_frames(&self);
            engine_unlock();
        }
        engine_lock();
        if (owner == &self && !booted && !booting && window) {
            booting = 1;
            engine_unlock();
            boot(app);
            engine_lock();
            booting = 0;
            booted = 1;
            /* the engine may have moved to another activity meanwhile: start it there */
            if (focused) { audio_start(); set_paused(0); }
            if (owner != &self) ALooper_wake(owner->app->looper);
        }
        if (owner == &self && booted && focused && window) {
            uint64_t t0 = mono_ms();
            if (drop_pending && t0 >= drop_release_at) drop_release();
            if (!rg_tick()) { quitting = 1; ANativeActivity_finish(app->activity); }
            if (rg_host_players() != cfg_players) { cfg_players = rg_host_players(); save_config(); }
            uint64_t t1 = mono_ms();
#ifdef RG_DIAG
            dg.ticks++;
            if (t1 - t0 > dg.tick_max) dg.tick_max = (unsigned)(t1 - t0);
            diag_second(t1);
#endif
            audio_pump();
            if (!controls_visible && mono_ms() - last_pad_input > 30000) controls_visible = 1;
            present();
            static unsigned ticks;
            static uint64_t tick_ms, present_ms;
            tick_ms += t1 - t0;
            present_ms += mono_ms() - t1;
            if (++ticks % 1024 == 0) {
                host_log("tick %u: engine %llu ms, audio+present %llu ms per 1024 ticks, wait screen %d", ticks,
                         (unsigned long long)tick_ms, (unsigned long long)present_ms, rg_in_wait_screen());
                tick_ms = present_ms = 0;
            }
        }
        busy = wants_frames(&self);
        engine_unlock();
    }
}
