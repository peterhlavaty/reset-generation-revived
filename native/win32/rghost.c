/* rghost.dll: "host a game" for the Windows (Java) build. Loaded by rg.HostServer into the same
 * 32-bit JVM as the real engine DLL, it does in native memory what native/rt/server.c does for
 * the recompiled engine (see docs/MULTIPLAYER.md, "Phase 0 result"):
 *   - the engine's LAN host screen (0x100caa80) is built and made the current screen; its entry
 *     creates the server-mode controller (0x1000ecd0) and a TCP acceptor;
 *   - the acceptor's setController (vtable 0x10188b64, slot 1) is replaced so it uses a fake
 *     listener whose accept() hands out "HostConn" objects (vtables implemented here);
 *   - those connections carry channel 0x16 through the relay: the room's GameRoom bot forwards
 *     each remote player's messages to us as [u16 pid][zlib message] and back.
 * Unlike the headless server of server.c, the host's own engine is the game server *and* the
 * local player 0, exactly like the original LAN mode: no second engine instance is needed.
 *
 * Everything engine-facing runs on the engine thread (HostServer.pump from tickRG); the relay
 * socket has a reader thread that only queues events. */
#include <jni.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>

#define TC __attribute__((thiscall))
#define MAX_SESS 64                /* slots are never reused: old connections may still be polled */
#define CH_GAME 0x16
#define IMAGE_BASE 0x10000000u

static uint32_t delta;                 /* actual DLL base - preferred base */
#define A(x) ((uint32_t)(x) + delta)
#define P(x) ((void *)(uintptr_t)A(x))

static void lg(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fputs("rghost: ", stderr);
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    fflush(stderr);
    va_end(ap);
}

/* engine functions */
typedef void *(__cdecl *new_fn)(uint32_t);
typedef void (TC *push_back_fn)(void *list, void *val);
typedef void (TC *host_screen_ctor_fn)(void *screen, uint32_t ctx, void *list);
typedef void (TC *set_screen_fn)(void *flow, void *screen);
typedef void (TC *msg_ctor_fn)(void *msg, uint32_t cap);
typedef void (TC *write_fn)(void *ostream, const void *buf, uint32_t len);
static void *eng_new(uint32_t n) { void *p = ((new_fn)P(0x1014f476))(n); memset(p, 0, n); return p; }
#define LD32(a) (*(volatile uint32_t *)(uintptr_t)(a))

/* ------------------------------------------------------------------ relay link (WebSocket) */
typedef struct ev { struct ev *next; int kind, pid, len; uint8_t data[]; } ev;
enum { EV_JOIN, EV_LEAVE, EV_DATA, EV_CLOSED };

static CRITICAL_SECTION lock, send_lock;
static ev *ev_head, *ev_tail;
static SOCKET ws = INVALID_SOCKET;
static volatile int ws_up;
static volatile int serving;             /* host() until the engine closes our listener (l_close) */
static char url[256], room[72];
static int remote_players;

static void push_ev(int kind, int pid, const void *data, int len) {
    ev *e = (ev *)calloc(1, sizeof *e + (size_t)(len > 0 ? len : 0));
    e->kind = kind;
    e->pid = pid;
    e->len = len;
    if (len > 0) memcpy(e->data, data, (size_t)len);
    EnterCriticalSection(&lock);
    if (ev_tail) ev_tail->next = e; else ev_head = e;
    ev_tail = e;
    LeaveCriticalSection(&lock);
}
static ev *pop_ev(void) {
    EnterCriticalSection(&lock);
    ev *e = ev_head;
    if (e) { ev_head = e->next; if (!ev_head) ev_tail = NULL; }
    LeaveCriticalSection(&lock);
    return e;
}
static int send_all(SOCKET s, const uint8_t *p, size_t n) {
    while (n) { int k = send(s, (const char *)p, (int)n, 0); if (k <= 0) return 0; p += k; n -= (size_t)k; }
    return 1;
}
static int recv_all(SOCKET s, uint8_t *p, size_t n) {
    while (n) { int k = recv(s, (char *)p, (int)n, 0); if (k <= 0) return 0; p += k; n -= (size_t)k; }
    return 1;
}
static int ws_send(int opcode, const void *data, size_t len) {
    uint8_t hdr[14], mask[4];
    size_t h = 0;
    uint32_t r = (uint32_t)rand() * 2654435761u ^ GetTickCount();
    memcpy(mask, &r, 4);
    hdr[h++] = (uint8_t)(0x80 | opcode);
    if (len < 126) hdr[h++] = (uint8_t)(0x80 | len);
    else { hdr[h++] = 0x80 | 126; hdr[h++] = (uint8_t)(len >> 8); hdr[h++] = (uint8_t)len; }
    memcpy(hdr + h, mask, 4);
    h += 4;
    uint8_t *buf = (uint8_t *)malloc(h + len);
    memcpy(buf, hdr, h);
    for (size_t i = 0; i < len; i++) buf[h + i] = ((const uint8_t *)data)[i] ^ mask[i & 3];
    EnterCriticalSection(&send_lock);
    int ok = ws != INVALID_SOCKET && send_all(ws, buf, h + len);
    LeaveCriticalSection(&send_lock);
    free(buf);
    return ok;
}
static void json_str(const char *j, const char *key, char *out, size_t n) {
    char pat[40];
    snprintf(pat, sizeof pat, "\"%s\":\"", key);
    const char *p = strstr(j, pat);
    out[0] = 0;
    if (!p) return;
    p += strlen(pat);
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < n) { if (*p == '\\' && p[1]) p++; out[i++] = *p++; }
    out[i] = 0;
}
static int json_int(const char *j, const char *key, int dflt) {
    char pat[40];
    snprintf(pat, sizeof pat, "\"%s\":", key);
    const char *p = strstr(j, pat);
    return p ? atoi(p + strlen(pat)) : dflt;
}

static DWORD WINAPI reader(void *arg) {
    (void)arg;
    uint8_t *msg = (uint8_t *)malloc(1 << 20);
    for (;;) {
        uint8_t h[2];
        if (!recv_all(ws, h, 2)) break;
        int op = h[0] & 15;
        uint64_t len = h[1] & 127;
        if (len == 126) { uint8_t e[2]; if (!recv_all(ws, e, 2)) break; len = (uint64_t)e[0] << 8 | e[1]; }
        else if (len == 127) { uint8_t e[8]; if (!recv_all(ws, e, 8)) break; len = 0; for (int i = 0; i < 8; i++) len = len << 8 | e[i]; }
        if (len >= (1 << 20) - 1) break;
        if (!recv_all(ws, msg, (size_t)len)) break;
        if (op == 1) {
            msg[len] = 0;
            char t[24];
            json_str((char *)msg, "t", t, sizeof t);
            if (!strcmp(t, "player_joined")) push_ev(EV_JOIN, json_int((char *)msg, "pid", -1), NULL, 0);
            else if (!strcmp(t, "player_left")) push_ev(EV_LEAVE, json_int((char *)msg, "pid", -1), NULL, 0);
            else if (!strcmp(t, "pong")) {}
            else lg("relay %s", (char *)msg);
        } else if (op == 2) {
            if (len >= 2) push_ev(EV_DATA, msg[0] | msg[1] << 8, msg + 2, (int)len - 2);
        } else if (op == 9) {
            ws_send(10, msg, (size_t)len);
        } else if (op == 8) {
            break;
        }
    }
    free(msg);
    ws_up = 0;
    push_ev(EV_CLOSED, 0, NULL, 0);
    return 0;
}

static int ws_connect(void) {
    char host[200], path[200] = "/";
    int port = 80;
    if (strncmp(url, "ws://", 5) != 0) { lg("only ws:// relays are supported (got %s)", url); return 0; }
    const char *p = url + 5, *slash = strchr(p, '/'), *colon = strchr(p, ':');
    size_t hl = slash ? (size_t)(slash - p) : strlen(p);
    if (colon && (!slash || colon < slash)) { hl = (size_t)(colon - p); port = atoi(colon + 1); }
    snprintf(host, sizeof host, "%.*s", (int)hl, p);
    if (slash) snprintf(path, sizeof path, "%s", slash);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_socktype = SOCK_STREAM;
    char ports[8];
    snprintf(ports, sizeof ports, "%d", port);
    if (getaddrinfo(host, ports, &hints, &res) != 0 || !res) { lg("cannot resolve %s", host); return 0; }
    SOCKET s = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (s == INVALID_SOCKET || connect(s, res->ai_addr, (int)res->ai_addrlen) != 0) {
        lg("cannot connect to %s:%d", host, port);
        if (s != INVALID_SOCKET) closesocket(s);
        freeaddrinfo(res);
        return 0;
    }
    freeaddrinfo(res);
    int one = 1;
    setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
    char req[600];
    snprintf(req, sizeof req,
             "GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
             "Sec-WebSocket-Key: UmVzZXRHZW5lcmF0aW9uIQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
             path, host, port);
    char resp[1024];
    size_t got = 0;
    if (!send_all(s, (const uint8_t *)req, strlen(req))) { closesocket(s); return 0; }
    while (got < sizeof resp - 1) {
        int k = recv(s, resp + got, 1, 0);
        if (k <= 0) break;
        got += (size_t)k;
        resp[got] = 0;
        if (got >= 4 && !memcmp(resp + got - 4, "\r\n\r\n", 4)) break;
    }
    if (!strstr(resp, " 101 ")) { lg("websocket upgrade refused"); closesocket(s); return 0; }
    ws = s;
    ws_up = 1;
    CloseHandle(CreateThread(NULL, 0, reader, NULL, 0, NULL));
    char json[300];
    snprintf(json, sizeof json, "{\"t\":\"hello\",\"v\":1,\"name\":\"RGHost\"}");
    ws_send(1, json, strlen(json));
    if (serving) {
        snprintf(json, sizeof json, "{\"t\":\"serve\",\"room\":\"%s\",\"players\":%d}", room, remote_players);
        ws_send(1, json, strlen(json));
    }
    lg("hosting room %s on %s for %d remote player(s)", room, url, remote_players);
    return 1;
}

/* ------------------------------------------------------------------ sessions */
typedef struct { int used, pid, accepted, closed; uint8_t *obj; ev *in_head, *in_tail; } sess_t;
static sess_t sess[MAX_SESS];

static sess_t *by_pid(int pid) {
    sess_t *r = NULL;     /* the newest session with this pid */
    for (int i = 0; i < MAX_SESS; i++) if (sess[i].used && sess[i].pid == pid) r = &sess[i];
    return r;
}
static sess_t *sess_of(void *obj) { uint32_t i = ((uint32_t *)obj)[1]; return i < MAX_SESS ? &sess[i] : NULL; }

static void to_player(sess_t *s, const uint8_t *data, uint32_t len) {
    uint8_t *buf = (uint8_t *)malloc((size_t)len + 2);
    buf[0] = (uint8_t)s->pid;
    buf[1] = (uint8_t)(s->pid >> 8);
    memcpy(buf + 2, data, len);
    ws_send(2, buf, (size_t)len + 2);
    free(buf);
}

/* ------------------------------------------------------------------ fake MT objects */
static void bad(int slot, void *t, void *ret) {
    lg("FATAL: unimplemented MT object method (vtable slot 0x%x, this=%p, ret=%p)", slot, t, ret);
    MessageBoxA(NULL, "rghost: unimplemented engine call, see the log", "Reset Generation", MB_OK);
    ExitProcess(3);
}
#define B(n) static void TC bad##n(void *t) { bad(n * 4, t, __builtin_return_address(0)); }
B(0) B(1) B(2) B(3) B(4) B(5) B(6) B(7) B(8) B(9) B(10) B(11) B(12) B(13) B(14) B(15) B(16) B(17) B(18) B(19)
B(20) B(21) B(22) B(23) B(24) B(25) B(26) B(27) B(28) B(29) B(30) B(31) B(32) B(33) B(34) B(35) B(36) B(37) B(38) B(39)
static void *const bad_slots[40] = {bad0, bad1, bad2, bad3, bad4, bad5, bad6, bad7, bad8, bad9, bad10, bad11, bad12, bad13,
    bad14, bad15, bad16, bad17, bad18, bad19, bad20, bad21, bad22, bad23, bad24, bad25, bad26, bad27, bad28, bad29, bad30,
    bad31, bad32, bad33, bad34, bad35, bad36, bad37, bad38, bad39};

static void *listener_vt[40], *conn_vt[40], *addr_vt[40];
static uint32_t listener_obj[8] = {(uint32_t)(uintptr_t)listener_vt};

/* acceptor (0x100df500): setController(controller) -> our listener instead of tcp://:12345 */
static int TC acc_set_controller(uint8_t *acc, uint32_t ctrl) {
    *(uint32_t *)(acc + 4) = ctrl;
    *(uint32_t *)(acc + 8) = (uint32_t)(uintptr_t)listener_obj;
    acc[0x18] = 1;
    lg("controller %08x attached to the acceptor %p", ctrl, acc);
    return 1;
}
static void *TC any_dtor(void *t, uint32_t flags) { (void)flags; return t; }
static void *TC l_accept(void *t) {
    (void)t;
    for (int i = 0; i < MAX_SESS; i++) {
        sess_t *s = &sess[i];
        if (s->used && !s->accepted) {
            s->accepted = 1;
            s->obj = (uint8_t *)calloc(1, 0x400);
            ((uint32_t *)s->obj)[0] = (uint32_t)(uintptr_t)conn_vt;
            ((uint32_t *)s->obj)[1] = (uint32_t)i;
            lg("accepted player %d (conn %p)", s->pid, s->obj);
            return s->obj;
        }
    }
    return NULL;
}
/* The engine's game server is gone (match over, or the host left the host screen): the relay
 * must stop sending Quick Match players to this room. */
static void TC l_close(void *t) {
    (void)t;
    lg("listener closed");
    serving = 0;
    for (int i = 0; i < MAX_SESS; i++) if (sess[i].used) sess[i].closed = 1;   /* that game's players */
    if (ws_up) ws_send(1, "{\"t\":\"unserve\"}", 15);
}
static void *TC c_dtor(void *t, uint32_t flags) { sess_t *s = sess_of(t); lg("conn of player %d destroyed", s ? s->pid : -1); return t; }

/* conn.address(): callers only print it (string object at +0xc -> header -> char* at +0xc) */
static int TC addr_nop(void *t, uint32_t a) { (void)t; (void)a; return 0; }
static void *TC c_address(void *t) {
    sess_t *s = sess_of(t);
    static uint32_t addr[MAX_SESS][8], hdr[MAX_SESS][8];
    static char name[MAX_SESS][32];
    int i = (int)(s - sess);
    snprintf(name[i], sizeof name[i], "relay:player%d", s->pid);
    hdr[i][3] = (uint32_t)(uintptr_t)name[i];
    addr[i][0] = (uint32_t)(uintptr_t)addr_vt;
    addr[i][3] = (uint32_t)(uintptr_t)hdr[i];
    return addr[i];
}
static int TC c_state(void *t) { (void)t; return 3; }
static int TC c_alive(void *t) { sess_t *s = sess_of(t); return s && !s->closed; }
static void TC c_close(void *t) {
    sess_t *s = sess_of(t);
    if (s) { lg("engine closes the connection of player %d", s->pid); s->closed = 1; }
}
static int TC c_send(void *t, uint32_t ch, const uint8_t *ptr, uint32_t len) {
    sess_t *s = sess_of(t);
    if (s && ch == CH_GAME && len <= 1u << 20) {
        if (getenv("RT_NET_DEBUG")) lg("-> player %d: %u bytes", s->pid, len);
        to_player(s, ptr, len);
    }
    return 1;
}
/* recv(channel, flag) -> MT message object (ByteStream at +0xc) or 0 */
static void *TC c_recv(void *t, uint32_t ch, uint32_t flag) {
    (void)flag;
    sess_t *s = sess_of(t);
    if (!s || ch != CH_GAME || !s->in_head) return NULL;
    ev *e = s->in_head;
    s->in_head = e->next;
    if (!s->in_head) s->in_tail = NULL;
    /* never freed by us: the engine may keep messages (the real connection pools them) */
    uint8_t *msg = (uint8_t *)eng_new(0x48);
    ((msg_ctor_fn)P(0x1011f700))(msg, (uint32_t)e->len + 0x40);
    uint8_t *ostream = msg + 0xc + 8;
    ((write_fn)(uintptr_t)LD32(LD32(ostream)))(ostream, e->data, (uint32_t)e->len);
    if (getenv("RT_NET_DEBUG")) lg("<- player %d: %d bytes", s->pid, e->len);
    free(e);
    return msg;
}

/* ------------------------------------------------------------------ hooks */
static volatile uint32_t menu_base;      /* the main menu flow object (constructor 0x1008f500) */

static void patch(uint32_t addr, const void *bytes, size_t n) {
    DWORD old;
    VirtualProtect((void *)(uintptr_t)addr, n, PAGE_EXECUTE_READWRITE, &old);
    memcpy((void *)(uintptr_t)addr, bytes, n);
    VirtualProtect((void *)(uintptr_t)addr, n, old, &old);
    FlushInstructionCache(GetCurrentProcess(), (void *)(uintptr_t)addr, n);
}

/* 0x1008f500: push -1; push 0x1016b7fe (7 bytes) -> jmp to a trampoline that records ECX */
static int hook_menu_ctor(void) {
    uint32_t site = A(0x1008f500);
    static const uint8_t expect[7] = {0x6a, 0xff, 0x68, 0xfe, 0xb7, 0x16, 0x10};
    uint8_t want[7];
    memcpy(want, expect, 7);
    uint32_t imm = A(0x1016b7fe);
    memcpy(want + 3, &imm, 4);
    if (memcmp((void *)(uintptr_t)site, want, 7) != 0) { lg("menu constructor bytes differ; not hooked"); return 0; }
    uint8_t *t = (uint8_t *)VirtualAlloc(NULL, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    uint8_t *q = t;
    uint32_t mb = (uint32_t)(uintptr_t)&menu_base;
    *q++ = 0x89; *q++ = 0x0d; memcpy(q, &mb, 4); q += 4;           /* mov [menu_base], ecx */
    memcpy(q, want, 7); q += 7;                                     /* original prologue */
    *q++ = 0xe9; int32_t rel = (int32_t)(site + 7 - (uint32_t)(uintptr_t)(q + 4)); memcpy(q, &rel, 4); q += 4;
    uint8_t j[7] = {0xe9, 0, 0, 0, 0, 0x90, 0x90};
    rel = (int32_t)((uint32_t)(uintptr_t)t - (site + 5));
    memcpy(j + 1, &rel, 4);
    patch(site, j, 7);
    return 1;
}

/* Engine bug fix (all instances, not only hosts). PlaceTileUIState::enter (0x100b2c20) sets the
 * state's tile-layer grid (this+0x84 = this->layer(2), 0x1008cbc0) only for a player that is
 * still in the game; for one that has surrendered or been knocked out (player state 5) it returns
 * early and leaves +0x84 uninitialised, yet update (0x100b5df0 -> 0x100b5290) still copies board
 * snapshots into it. Depending on the stale heap value that crashes (access violation at
 * 0x100b5311/0x100b5325) or silently corrupts the heap (black screen later), typically for the
 * last player to surrender in a 3-4 player match. Both early exits now set +0x84 first. */
static int fix_place_tile_enter(void) {
    uint32_t site = A(0x100b2c66), done = A(0x100b2e37);
    static const uint8_t expect[26] = {0x83, 0xf8, 0x05, 0x0f, 0x84, 0xc8, 0x01, 0x00, 0x00,   /* cmp eax,5; je done */
                                       0x8b, 0x4e, 0x10, 0x8b, 0x51, 0x2c, 0x8b, 0x0c, 0x82,
                                       0x85, 0xc9, 0x0f, 0x84, 0xb7, 0x01, 0x00, 0x00};      /* test ecx,ecx; je done */
    if (memcmp((void *)(uintptr_t)site, expect, sizeof expect) != 0) { lg("PlaceTileUIState bytes differ; fix not applied"); return 0; }
    uint8_t *t = (uint8_t *)VirtualAlloc(NULL, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (!t) return 0;
    uint8_t *q = t;
    uint32_t layer = A(0x1008cbc0);
    *q++ = 0x50;                                                    /* push eax */
    *q++ = 0x6a; *q++ = 0x02;                                       /* push 2 */
    *q++ = 0x89; *q++ = 0xf1;                                       /* mov ecx, esi */
    *q++ = 0xb8; memcpy(q, &layer, 4); q += 4;                      /* mov eax, layer */
    *q++ = 0xff; *q++ = 0xd0;                                       /* call eax (thiscall, ret 4) */
    *q++ = 0x89; *q++ = 0x86; *q++ = 0x84; *q++ = 0; *q++ = 0; *q++ = 0;   /* mov [esi+0x84], eax */
    *q++ = 0x58;                                                    /* pop eax */
    *q++ = 0x68; memcpy(q, &done, 4); q += 4;                       /* push done */
    *q++ = 0xc3;                                                    /* ret */
    FlushInstructionCache(GetCurrentProcess(), t, (size_t)(q - t));
    int32_t r1 = (int32_t)((uint32_t)(uintptr_t)t - (site + 9));   /* je at site+3 */
    int32_t r2 = (int32_t)((uint32_t)(uintptr_t)t - (site + 26));  /* je at site+20 */
    patch(site + 5, &r1, 4);
    patch(site + 22, &r2, 4);
    return 1;
}

static int installed;
static int install(void) {
    if (installed) return installed > 0;
    HMODULE m = GetModuleHandleA("ResetGeneration-Win32-Web.dll");
    if (!m) { lg("engine DLL not loaded"); installed = -1; return 0; }
    delta = (uint32_t)(uintptr_t)m - IMAGE_BASE;
    if (fix_place_tile_enter()) lg("engine fix: PlaceTileUIState layer for players out of the game");
    for (int i = 0; i < 40; i++) listener_vt[i] = conn_vt[i] = addr_vt[i] = bad_slots[i];
    listener_vt[0] = (void *)any_dtor;
    listener_vt[0x4c / 4] = (void *)l_accept;
    listener_vt[0x5c / 4] = (void *)l_close;
    conn_vt[0] = (void *)c_dtor;
    conn_vt[0x14 / 4] = (void *)c_address;
    conn_vt[0x18 / 4] = (void *)c_state;
    conn_vt[0x2c / 4] = (void *)c_alive;
    conn_vt[0x5c / 4] = (void *)c_close;
    conn_vt[0x68 / 4] = (void *)c_send;
    conn_vt[0x7c / 4] = (void *)c_recv;
    addr_vt[0x14 / 4] = (void *)addr_nop;
    if (LD32(A(0x10188b68)) != A(0x100df500)) { lg("acceptor vtable differs; not installed"); installed = -1; return 0; }
    if (!hook_menu_ctor()) { installed = -1; return 0; }
    lg("installed (engine base %08x)", A(IMAGE_BASE));
    installed = 1;
    return 1;
}

/* What the menus would do for "host a LAN game": build the LAN host screen (a hero list whose
 * length selects the mission and the player count) and make it the current screen. */
static int host_game(int players) {
    if (!menu_base) { lg("menu not ready yet"); return 0; }
    /* the acceptor's setController (vtable slot 1) -> our listener; only the LAN host screen uses it */
    static int acc_patched;
    if (!acc_patched) { uint32_t fn = (uint32_t)(uintptr_t)acc_set_controller; patch(A(0x10188b68), &fn, 4); acc_patched = 1; }
    uint32_t ctx = LD32(menu_base + 0x10);
    void *list = eng_new(0xc);
    for (int i = 0; i < players; i++) {
        uint32_t *val = (uint32_t *)eng_new(4);
        *val = (uint32_t)i;
        ((push_back_fn)P(0x10104a40))(list, val);
    }
    void *screen = eng_new(0x3c);
    ((host_screen_ctor_fn)P(0x100caa80))(screen, ctx, list);
    ((set_screen_fn)(uintptr_t)LD32(LD32(menu_base) + 0x14))((void *)(uintptr_t)menu_base, screen);
    lg("host screen %p for %d players", screen, players);
    return 1;
}

/* ------------------------------------------------------------------ JNI (rg.HostServer) */
static int hosting;
static DWORD last_ping, retry;

/* A match is running while a remote player's connection is alive. */
static int match_live(void) {
    for (int i = 0; i < MAX_SESS; i++) if (sess[i].used && sess[i].accepted && !sess[i].closed) return 1;
    return 0;
}

JNIEXPORT jboolean JNICALL Java_rg_HostServer_install(JNIEnv *env, jclass c) {
    (void)env; (void)c;
    static int once;
    if (!once) {
        once = 1;
        InitializeCriticalSection(&lock);
        InitializeCriticalSection(&send_lock);
        WSADATA w;
        WSAStartup(MAKEWORD(2, 2), &w);
    }
    return (jboolean)install();
}

/* remote players connected to the current game */
JNIEXPORT jint JNICALL Java_rg_HostServer_players(JNIEnv *env, jclass c) {
    (void)env; (void)c;
    int n = 0;
    for (int i = 0; i < MAX_SESS; i++) if (sess[i].used && !sess[i].closed) n++;
    return n;
}

JNIEXPORT jboolean JNICALL Java_rg_HostServer_serving(JNIEnv *env, jclass c) { (void)env; (void)c; return serving != 0; }

JNIEXPORT jboolean JNICALL Java_rg_HostServer_menuReady(JNIEnv *env, jclass c) { (void)env; (void)c; return menu_base != 0; }

JNIEXPORT jboolean JNICALL Java_rg_HostServer_host(JNIEnv *env, jclass c, jstring jurl, jstring jroom, jint players) {
    (void)c;
    if (installed <= 0 || match_live()) return JNI_FALSE;
    const char *u = (*env)->GetStringUTFChars(env, jurl, NULL), *r = (*env)->GetStringUTFChars(env, jroom, NULL);
    snprintf(url, sizeof url, "%s", u);
    snprintf(room, sizeof room, "%s", r);
    (*env)->ReleaseStringUTFChars(env, jurl, u);
    (*env)->ReleaseStringUTFChars(env, jroom, r);
    if (players < 2) players = 2;
    if (players > 4) players = 4;
    remote_players = players - 1;
    for (int i = 0; i < MAX_SESS; i++) if (sess[i].used) sess[i].closed = 1;    /* a previous game's players */
    if (!host_game(players)) return JNI_FALSE;
    if (hosting && ws_up) {      /* hosting again: offer the room on the open relay connection */
        char json[300];
        snprintf(json, sizeof json, "{\"t\":\"serve\",\"room\":\"%s\",\"players\":%d}", room, remote_players);
        ws_send(1, json, strlen(json));
        lg("hosting room %s again", room);
    }
    hosting = 1;
    serving = 1;
    return JNI_TRUE;
}

JNIEXPORT void JNICALL Java_rg_HostServer_pump(JNIEnv *env, jclass c) {
    (void)env; (void)c;
    if (!hosting) return;
    DWORD now = GetTickCount();
    if (!ws_up && ws == INVALID_SOCKET && (now - retry > 2000 || !retry)) { retry = now ? now : 1; ws_connect(); }
    if (ws_up && now - last_ping > 15000) { last_ping = now; ws_send(1, "{\"t\":\"ping\"}", 12); }
    ev *e;
    while ((e = pop_ev())) {
        sess_t *s = by_pid(e->pid);
        if (e->kind == EV_JOIN) {
            if (s && s->closed) s = NULL;      /* the same relay pid in a new game */
            if (!s) {
                for (int i = 0; i < MAX_SESS && !s; i++) if (!sess[i].used) s = &sess[i];
                if (s) { memset(s, 0, sizeof *s); s->used = 1; s->pid = e->pid; lg("player %d joined", e->pid); }
            }
            free(e);
        } else if (e->kind == EV_LEAVE) {
            if (s) { s->closed = 1; lg("player %d left", e->pid); }
            free(e);
        } else if (e->kind == EV_DATA && s) {
            e->next = NULL;
            if (s->in_tail) s->in_tail->next = e; else s->in_head = e;
            s->in_tail = e;
        } else if (e->kind == EV_CLOSED) {
            lg("relay connection lost");
            if (ws != INVALID_SOCKET) { closesocket(ws); ws = INVALID_SOCKET; }
            for (int i = 0; i < MAX_SESS; i++) if (sess[i].used) sess[i].closed = 1;
            free(e);
        } else free(e);
    }
}
