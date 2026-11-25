/* Platform services for the desktop game (native/desktop): logging, clock, sleep. */
#include "host.h"
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

static FILE *log_file;

void rg_desktop_init(const char *log_path) {
    if (log_path && *log_path) log_file = fopen(log_path, "w");
#ifdef _WIN32
    timeBeginPeriod(1);   /* the engine paces itself with Sleep(): 1 ms, not 15.6 ms steps */
#endif
}

void host_log(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
    if (log_file) {
        va_start(ap, fmt);
        vfprintf(log_file, fmt, ap);
        va_end(ap);
        fputc('\n', log_file);
        fflush(log_file);
    }
}

void host_fatal(const char *msg) {
    host_log("fatal: %s", msg);
#ifdef _WIN32
    MessageBoxA(NULL, msg, "Reset Generation", MB_ICONERROR | MB_OK);
#endif
    exit(1);
}

uint32_t host_time_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint32_t)((uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u);
}

void host_sleep_ms(uint32_t ms) {
#ifdef _WIN32
    Sleep(ms);
#else
    usleep(ms * 1000);
#endif
}
