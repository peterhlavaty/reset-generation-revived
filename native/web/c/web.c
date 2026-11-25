/* Platform services of the browser build (host.h) and its choice of relay. The relay links are
 * the page's WebSockets (native/web/src/link.rs); a page cannot listen or broadcast, so there are
 * no LAN games: every setting but a relay's address means the public server. */
#include "engine.h"
#include "host.h"
#include "link.h"
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

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

static const char *relay(const char *setting) {
    if (setting && (!strncmp(setting, "ws://", 5) || !strncmp(setting, "wss://", 6))) return setting;
    if (setting && !strcmp(setting, "lan")) host_log("net: there are no LAN games in a web browser: the public server");
    return RG_ONLINE_SERVER;
}
const char *relay_for_joining(const char *setting, char *buf, size_t n) { (void)buf, (void)n; return relay(setting); }
const char *relay_for_hosting(const char *setting, const char *nick, char *buf, size_t n) { (void)nick, (void)buf, (void)n; return relay(setting); }
int relay_reachable(const char *url) { (void)url; return 1; }
void rg_set_relay_starter(relay_starter fn) { (void)fn; }
