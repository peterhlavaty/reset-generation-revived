/* Platform services of the browser build (host.h) and its stand-in for the relay links: a page
 * cannot open TCP connections, so online play is off (net.c and server.c see that threads cannot
 * start and report "cannot connect" / "cannot host"). */
#include "host.h"
#include "link.h"
#include <stdarg.h>
#include <stdio.h>

/* from the Rust host (native/web/src/lib.rs) */
void rg_web_log(const char *line, size_t len);
_Noreturn void rg_web_abort(const char *why);
double rg_web_now_ms(void);

void host_log(const char *fmt, ...) {
    char buf[1024];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    rg_web_log(buf, (size_t)n < sizeof buf ? (size_t)n : sizeof buf - 1);
}

void host_fatal(const char *msg) { rg_web_abort(msg); }

uint32_t host_time_ms(void) { return (uint32_t)(uint64_t)rg_web_now_ms(); }
void host_sleep_ms(uint32_t ms) { (void)ms; }   /* the engine runs on the virtual clock */

/* the engine's clock, for the host's pacing */
uint32_t rg_web_clock(void) { return rt_now_ms(); }

rt_link *link_open(const char *url, const char *who) {
    host_log("%s: cannot connect to %s from a web page", who, url);
    return NULL;
}
void link_ref(rt_link *l) { (void)l; }
void link_release(rt_link *l) { (void)l; }
int link_send(rt_link *l, int opcode, const void *data, size_t len) { (void)l, (void)opcode, (void)data, (void)len; return 0; }
int link_read(rt_link *l, uint8_t *buf, size_t cap, size_t *len) { (void)l, (void)buf, (void)cap, (void)len; return 0; }
void link_close(rt_link *l) { (void)l; }
const char *relay_for_joining(const char *setting, char *buf, size_t n) { (void)setting, (void)buf, (void)n; return NULL; }
const char *relay_for_hosting(const char *setting, const char *nick, char *buf, size_t n) { (void)setting, (void)nick, (void)buf, (void)n; return NULL; }
int relay_reachable(const char *url) { (void)url; return 0; }
void rg_set_relay_starter(relay_starter fn) { (void)fn; }
