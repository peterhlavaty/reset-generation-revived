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
#define MAX_SESS 4096              /* slots are never reused: the engine never frees a connection */
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
enum { EV_JOIN, EV_LEAVE, EV_LOST, EV_DATA, EV_CLOSED };

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
            if (!strcmp(t, "player_joined")) {
                char name[32];
                json_str((char *)msg, "name", name, sizeof name);
                push_ev(EV_JOIN, json_int((char *)msg, "pid", -1), name, (int)strlen(name) + 1);
            }
            else if (!strcmp(t, "player_left"))
                push_ev(json_int((char *)msg, "lost", 0) ? EV_LOST : EV_LEAVE, json_int((char *)msg, "pid", -1), NULL, 0);
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
typedef struct {
    int used, pid, accepted, closed;
    uint8_t *obj;
    ev *in_head, *in_tail;
    char name[32];          /* relay nickname */
    int key, slot;          /* the engine's player key and seat (0 = not yet known) */
    int checked;            /* rejoin: looked for a held seat */
    int away;               /* the relay connection dropped; the engine is not told (seat held) */
    DWORD away_since;
    int heir_of;            /* rejoin: index + 1 of the held session this one takes over */
} sess_t;
static sess_t sess[MAX_SESS];
static int nsess;                        /* slots handed out so far */

static sess_t *by_pid(int pid) {
    sess_t *r = NULL;     /* the newest session with this pid */
    for (int i = 0; i < nsess; i++) if (sess[i].used && sess[i].pid == pid) r = &sess[i];
    return r;
}
static sess_t *sess_of(void *obj) { uint32_t i = ((uint32_t *)obj)[1]; return i < (uint32_t)nsess ? &sess[i] : NULL; }

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
static uint32_t srv_ctrl;               /* the engine's server controller (pending joins at +0x94) */
static int TC acc_set_controller(uint8_t *acc, uint32_t ctrl) {
    *(uint32_t *)(acc + 4) = ctrl;
    srv_ctrl = ctrl;
    *(uint32_t *)(acc + 8) = (uint32_t)(uintptr_t)listener_obj;
    acc[0x18] = 1;
    lg("controller %08x attached to the acceptor %p", ctrl, acc);
    return 1;
}
static void *TC any_dtor(void *t, uint32_t flags) { (void)flags; return t; }
static void *TC l_accept(void *t) {
    (void)t;
    static DWORD polled;
    if (getenv("RT_NET_DEBUG") && GetTickCount() - polled > 10000) { polled = GetTickCount(); lg("accept polled"); }
    for (int i = 0; i < nsess; i++) {
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
    for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;   /* that game's players */
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
static uint32_t pending_entry(sess_t *s);
static void take_over(sess_t *s);
static int TC c_send(void *t, uint32_t ch, const uint8_t *ptr, uint32_t len) {
    sess_t *s = sess_of(t);
    /* not to a closed session (its relay pid may belong to a player who has come back since), nor
     * to one whose player is away (see track_seats) */
    if (s && !s->closed && !s->away && ch == CH_GAME && len <= 1u << 20) {
        if (getenv("RT_NET_DEBUG")) lg("-> player %d: %u bytes", s->pid, len);
        to_player(s, ptr, len);
        /* handshake state 4 sends the battle state, then gives up registering a key it has */
        if (s->heir_of) { uint32_t ent = pending_entry(s); if (ent && LD32(ent + 4) == 4) take_over(s); }
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

/* ------------------------------------------------------------------ the host's hero and name */
/* The LAN host screen's entry (0x100ca920) gives the host (player 0) a random hero:
 * 0x100ca9ce xor edx,edx; mov ecx,10; div ecx (edx = rand()%10, then mov ebp,edx at 0x100ca9d9).
 * It then builds the host's player setup (0x10042040: +0 name string, left empty; +4 hero id;
 * +8 hero object) and hands it to the controller at 0x100caa30..0x100caa3a (vt+0x6c), which sends
 * it to every client. Two detours: the roll keeps its RNG call but takes host_hero when that is
 * 0..9, and the setup gets the host's nickname (otherwise "Player" / empty in the end screens). */
static volatile uint32_t host_hero = 0xffffffffu;   /* 0..9 = that hero, anything else = random */
static char host_name[64];

typedef void *(TC *str_assign_fn)(void *str, const char *s);       /* 0x100f10b0, ret 4 */
static void __cdecl host_setup_fix(uint8_t *screen, uint8_t *setup) {
    (void)screen;
    if (setup && host_name[0]) ((str_assign_fn)P(0x100f10b0))(setup, host_name);
}

/* E9 rel32 at q (q is where the bytes will execute) */
static uint8_t *jmp_to(uint8_t *q, uint32_t target) {
    *q = 0xe9;
    int32_t rel = (int32_t)(target - ((uint32_t)(uintptr_t)q + 5));
    memcpy(q + 1, &rel, 4);
    return q + 5;
}

static int patch_host_player(void) {
    uint32_t roll = A(0x100ca9ce), give = A(0x100caa30);
    static const uint8_t roll_bytes[13] = {0x33, 0xd2, 0xb9, 0x0a, 0, 0, 0, 0xf7, 0xf1,  /* xor edx,edx; mov ecx,10; div ecx */
                                           0x6a, 0x14, 0x8b, 0xea};                      /* push 0x14; mov ebp,edx */
    static const uint8_t give_bytes[13] = {0x8b, 0x4e, 0x14, 0x8b, 0x01,                 /* mov ecx,[esi+0x14]; mov eax,[ecx] */
                                           0x6a, 0x00, 0x57, 0x6a, 0x00, 0xff, 0x50, 0x6c};  /* push 0; push edi; push 0; call [eax+0x6c] */
    if (memcmp((void *)(uintptr_t)roll, roll_bytes, 13) || memcmp((void *)(uintptr_t)give, give_bytes, 13)) {
        lg("LAN host entry bytes differ; the host keeps a random hero");
        return 0;
    }
    uint8_t *t = (uint8_t *)VirtualAlloc(NULL, 128, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (!t) return 0;
    uint8_t *q = t, *t2;
    uint32_t hh = (uint32_t)(uintptr_t)&host_hero, fn = (uint32_t)(uintptr_t)host_setup_fix;
    memcpy(q, roll_bytes, 9); q += 9;                               /* the original roll -> edx */
    *q++ = 0x8b; *q++ = 0x0d; memcpy(q, &hh, 4); q += 4;            /* mov ecx,[host_hero] */
    *q++ = 0x83; *q++ = 0xf9; *q++ = 0x09;                          /* cmp ecx,9 */
    *q++ = 0x77; *q++ = 0x02;                                       /* ja +2 (unsigned: keep the roll) */
    *q++ = 0x8b; *q++ = 0xd1;                                       /* mov edx,ecx */
    q = jmp_to(q, roll + 9);                                        /* -> push 0x14 */
    t2 = q;
    *q++ = 0x60;                                                    /* pushad */
    *q++ = 0x57; *q++ = 0x56;                                       /* push edi (setup); push esi (screen) */
    *q++ = 0xb8; memcpy(q, &fn, 4); q += 4; *q++ = 0xff; *q++ = 0xd0;   /* call host_setup_fix */
    *q++ = 0x83; *q++ = 0xc4; *q++ = 0x08;                          /* add esp,8 */
    *q++ = 0x61;                                                    /* popad */
    memcpy(q, give_bytes, 5); q += 5;                               /* the 5 bytes replaced at give */
    q = jmp_to(q, give + 5);
    FlushInstructionCache(GetCurrentProcess(), t, (size_t)(q - t));
    uint8_t j[9] = {0xe9, 0, 0, 0, 0, 0x90, 0x90, 0x90, 0x90};
    int32_t rel = (int32_t)((uint32_t)(uintptr_t)t - (roll + 5));
    memcpy(j + 1, &rel, 4);
    patch(roll, j, 9);
    rel = (int32_t)((uint32_t)(uintptr_t)t2 - (give + 5));
    memcpy(j + 1, &rel, 4);
    patch(give, j, 5);
    return 1;
}

/* Engine bug fix (all instances). The online user object (context service 0x39, vtable
 * 0x1017b020) is owned by the app context, but a casino room also puts it in its member map,
 * keyed by the player id the server gave (0x10047573). Leaving removes only the current id
 * (0x100482c0 -> 0x1000aee0), then deletes every remaining member (0x10095030 -> 0x10094e50).
 * If the id ever changed (a server that hands out a new id per casino visit), the stale entry
 * deletes the context's user object, and the next use of it (0x100ecf90, on the next tick after
 * a dropped connection) jumps through freed memory. The member delete in that loop
 * (0x10094eac: mov edx,[ecx]; push 1; call [edx]) now skips the context's own object. */
static int fix_member_clear(void) {
    uint32_t site = A(0x10094eac), ctxp = A(0x101b828c);
    static const uint8_t expect[8] = {0x8b, 0x11, 0x6a, 0x01, 0xff, 0x12, 0x46, 0x3b};
    if (memcmp((void *)(uintptr_t)site, expect, sizeof expect) != 0) { lg("member clear bytes differ; fix not applied"); return 0; }
    uint8_t *t = (uint8_t *)VirtualAlloc(NULL, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (!t) return 0;
    uint8_t *q = t;
    *q++ = 0xa1; memcpy(q, &ctxp, 4); q += 4;                       /* mov eax,[context] */
    *q++ = 0x85; *q++ = 0xc0;                                       /* test eax,eax */
    *q++ = 0x74; *q++ = 0x08;                                       /* jz del */
    *q++ = 0x3b; *q++ = 0x88; *q++ = 0xe4; *q++ = 0; *q++ = 0; *q++ = 0;   /* cmp ecx,[eax+0xe4] */
    *q++ = 0x74; *q++ = 0x06;                                       /* je skip */
    *q++ = 0x8b; *q++ = 0x11; *q++ = 0x6a; *q++ = 0x01; *q++ = 0xff; *q++ = 0x12;   /* del: the original delete */
    *q++ = 0xc3;                                                    /* skip: ret */
    FlushInstructionCache(GetCurrentProcess(), t, (size_t)(q - t));
    uint8_t c[6] = {0xe8, 0, 0, 0, 0, 0x90};                        /* call t; nop (returns to inc esi) */
    int32_t rel = (int32_t)((uint32_t)(uintptr_t)t - (site + 5));
    memcpy(c + 1, &rel, 4);
    patch(site, c, 6);
    return 1;
}

/* ------------------------------------------------------------------ rejoining players
 * The engine takes players into a running match (its acceptor is polled all match long; the
 * handshake in 0x1000d8c0 sends the battle state), but always as a new player with the next free
 * key (0x10023f40) and seat (0x10024c60): a player who came back to a 3-player match became
 * player 4, with no base and no hero, and their game crashed on their first move (0x100b4a90 finds
 * no hero). Giving them their old key and seat is not enough either: once the engine has seen the
 * connection drop (a leave command, 0x1005c530) it sends that player nothing any more.
 *
 * So when a player's relay connection drops mid-match the engine is not told: the session stays
 * "connected" (messages to it are dropped) like a player who does not move, as long as the match runs (HOLD_MS, 0 = no limit). If they
 * come back (same name), their new connection does the normal handshake, but with the
 * old key and seat: the seat is preset in the pending join before state 3 (which then skips the
 * seat search) and the key comes from key_alloc. The engine sends the battle state on the new
 * connection, does not register it (0x1005c340 skips a key it already has) and goes on using the
 * old connection object for that player, which from then on talks to the new relay player id.
 * The handshake would also dispatch a join command for the player (0x1000cc70, type 0xc); applied
 * to a player who is already in the game it replaces their player object and record (0x10010010,
 * 0x10027450) with empty ones on every machine, which crashed the other players a minute later.
 * But the player coming back needs it (without it their game stays black). So it goes only to
 * them (ctrl_command): the host's game does not apply it and no one else gets it. And on their
 * machine (rghost.dll is loaded by every Windows game) client_join keeps the player objects the
 * battle state already has (their game died a few minutes after the join without it).
 * Pending joins: ctrl+0x94, list of {prev, next, entry}; entry = {conn wrapper, state, info,
 * key +0xc, seat +0x10}; our connection object is at wrapper+0x7c. */
#define HOLD_MS 0 /* how long a seat is held, ms (0: until the match ends) */
static volatile int force_key;
static int rejoin_off;                   /* the hooks below could not be installed */
typedef int(TC *key_pool_fn)(void *pool);
static int TC key_alloc(uint8_t *pool) {
    int k;
    if (force_key > 0) { k = force_key; force_key = 0; }
    else k = ((key_pool_fn)P(0x10023eb0))(pool);
    ((uint8_t *)(uintptr_t)LD32(pool + 0x1c))[k] = 1;
    return k;
}
/* the server controller's command dispatch (vtable 0x101779a0 slot 9, 0x1000e150): takes a
 * reference-counted command by value and releases it */
static volatile int drop_join_key;
typedef void(TC *ctrl_cmd_fn)(void *ctrl, void *cmd);
typedef int(TC *obj_int_fn)(void *obj);
typedef void(TC *obj_unref_fn)(void *obj, int flag);
typedef void(TC *obj_delete_fn)(void *obj, int flags);
typedef void(TC *broadcast_fn)(void *sessions, void *cmd);
static void TC ctrl_command(void *ctrl, void *cmd) {
    uint32_t c = (uint32_t)(uintptr_t)cmd;
    if (drop_join_key > 0 && cmd && ((obj_int_fn)P(0x1000ce50))(cmd) == 0xc && (int)LD32(c + 0x44) == drop_join_key) {
        /* the command's recipients (player keys): array at +0x24, count at +0x2c */
        static uint32_t only[1];
        uint32_t keys = LD32(c + 0x24), n = LD32(c + 0x2c);
        only[0] = (uint32_t)drop_join_key;
        *(volatile uint32_t *)(uintptr_t)(c + 0x24) = (uint32_t)(uintptr_t)only;
        *(volatile uint32_t *)(uintptr_t)(c + 0x2c) = 1;
        ((broadcast_fn)P(0x1005c1d0))((void *)(uintptr_t)LD32((uint32_t)(uintptr_t)ctrl + 0x8c), cmd);
        *(volatile uint32_t *)(uintptr_t)(c + 0x24) = keys;
        *(volatile uint32_t *)(uintptr_t)(c + 0x2c) = n;
        lg("join command for key %d sent only to that player (coming back; %u recipients)", drop_join_key, n);
        drop_join_key = 0;
        ((obj_unref_fn)P(0x100f3e70))(cmd, 0);
        if (!((obj_int_fn)P(0x10112570))(cmd)) ((obj_delete_fn)(uintptr_t)LD32(LD32((uint32_t)(uintptr_t)cmd)))(cmd, 1);
        return;
    }
    ((ctrl_cmd_fn)P(0x1000e150))(ctrl, cmd);
}
static int hook_ctrl_command(void) {
    uint32_t slot = A(0x101779c4);
    if (LD32(slot) != A(0x1000e150)) return 0;
    uint32_t fn = (uint32_t)(uintptr_t)ctrl_command;
    patch(slot, &fn, 4);
    return 1;
}
/* The client controller's join handler (vtable 0x10188d18 slot 0x58, 0x100e16a0): for a key and seat
 * that are already in the game (records: game+0x1c -> +0xc count, +0x10 array by key; player
 * objects: game+0x14 -> +0x2c array by seat), put the old objects back after it. */
typedef void(TC *join_fn)(void *ctrl, void *cmd);
static uint32_t *join_rec(uint32_t g, int key) {
    uint32_t recs = LD32(g + 0x1c);
    return recs && key > 0 && key < (int)LD32(recs + 0xc) && LD32(recs + 0x10) ? (uint32_t *)(uintptr_t)(LD32(recs + 0x10) + 4u * key) : NULL;
}
static uint32_t *join_seat(uint32_t g, int seat) {
    uint32_t seats = LD32(g + 0x14);
    return seats && seat > 0 && seat <= 4 && LD32(seats + 0x2c) ? (uint32_t *)(uintptr_t)(LD32(seats + 0x2c) + 4u * seat) : NULL;
}
static void TC client_join(void *ctrl, void *cmd) {
    uint32_t g = (uint32_t)(uintptr_t)ctrl, c = (uint32_t)(uintptr_t)cmd;
    int key = cmd ? (int)LD32(c + 0x44) : 0, seat = cmd ? (int)LD32(c + 0x48) : 0;
    uint32_t *r = join_rec(g, key), *p = join_seat(g, seat);
    uint32_t old_rec = r ? *r : 0, old_player = p ? *p : 0;
    ((join_fn)P(0x100e16a0))(ctrl, cmd);          /* releases cmd */
    if (old_rec && old_player && (r = join_rec(g, key)) && (p = join_seat(g, seat))) {
        *r = old_rec;
        *p = old_player;
        lg("back in a match: key %d, seat %d kept", key, seat);
    }
}
static int hook_client_join(void) {
    uint32_t slot = A(0x10188d70);
    if (LD32(slot) != A(0x100e16a0)) return 0;
    uint32_t fn = (uint32_t)(uintptr_t)client_join;
    patch(slot, &fn, 4);
    return 1;
}
static int hook_key_alloc(void) {
    uint32_t site = A(0x10023f40);
    /* push esi; mov esi, ecx; call 0x10023eb0; mov ecx, [esi+0x1c]; mov byte [eax+ecx], 1; pop esi; ret */
    static const uint8_t head[4] = {0x56, 0x8b, 0xf1, 0xe8}, tail[9] = {0x8b, 0x4e, 0x1c, 0xc6, 0x04, 0x08, 0x01, 0x5e, 0xc3};
    if (memcmp((void *)(uintptr_t)site, head, 4) || memcmp((void *)(uintptr_t)(site + 8), tail, 9)) return 0;
    uint8_t j[5] = {0xe9};
    int32_t rel = (int32_t)((uint32_t)(uintptr_t)key_alloc - (site + 5));
    memcpy(j + 1, &rel, 4);
    patch(site, j, 5);
    return 1;
}
/* the pending join of a session, or 0 */
static uint32_t pending_entry(sess_t *s) {
    if (!srv_ctrl || !s->obj) return 0;
    for (uint32_t node = LD32(srv_ctrl + 0x94); node; node = LD32(node + 4)) {
        uint32_t ent = LD32(node + 8);
        if (ent && LD32(ent) && (uint8_t *)(uintptr_t)LD32(LD32(ent) + 0x7c) == s->obj) return ent;
    }
    return 0;
}
static void free_queue(sess_t *s) {
    while (s->in_head) { ev *n = s->in_head->next; free(s->in_head); s->in_head = n; }
    s->in_tail = NULL;
}
/* The new connection has its battle state: the held session now belongs to it. */
static void take_over(sess_t *s) {
    sess_t *old = &sess[s->heir_of - 1];
    s->heir_of = 0;
    lg("player %d (%s) is back in the match (was player %d)", s->pid, s->name, old->pid);
    free_queue(old);
    old->pid = s->pid;
    old->away = 0;
    old->in_head = s->in_head;           /* anything the new connection sent after its handshake */
    old->in_tail = s->in_tail;
    s->in_head = s->in_tail = NULL;
    s->used = 0;                         /* by_pid finds the held session now */
}
static void track_seats(void) {
    DWORD now = GetTickCount();
    for (int i = 0; i < nsess; i++) {
        sess_t *s = &sess[i];
        if (HOLD_MS && s->used && s->away && now - s->away_since > HOLD_MS) {
            s->away = 0;
            s->closed = 1;
            lg("player %d (%s) did not come back; the engine is told", s->pid, s->name);
        }
        if (!s->used || s->closed || s->key || !s->accepted) continue;
        uint32_t ent = pending_entry(s);
        if (!ent) continue;
        uint32_t st = LD32(ent + 4), key = LD32(ent + 0xc), slot = LD32(ent + 0x10);
        if (st == 3 && slot == 0 && !s->checked && s->name[0]) {
            s->checked = 1;
            for (int j = 0; j < nsess; j++) {
                sess_t *o = &sess[j];
                if (o != s && o->used && o->away && o->key > 0 && o->slot > 0 && !_stricmp(o->name, s->name)) {
                    o->away_since = now;         /* do not give the seat up during the handshake */
                    s->heir_of = j + 1;
                    *(volatile uint32_t *)(uintptr_t)(ent + 0x10) = (uint32_t)o->slot;
                    force_key = o->key;
                    drop_join_key = o->key;
                    lg("player %d (%s) comes back: key %d, seat %d", s->pid, s->name, o->key, o->slot);
                    break;
                }
            }
        }
        if (st == 4 && key) {
            s->key = (int)key;
            s->slot = (int)slot;
            lg("player %d (%s) is key %u, seat %u", s->pid, s->name, key, slot);
        }
    }
}

static int installed;
static int install(void) {
    if (installed) return installed > 0;
    HMODULE m = GetModuleHandleA("ResetGeneration-Win32-Web.dll");
    if (!m) { lg("engine DLL not loaded"); installed = -1; return 0; }
    delta = (uint32_t)(uintptr_t)m - IMAGE_BASE;
    if (fix_place_tile_enter()) lg("engine fix: PlaceTileUIState layer for players out of the game");
    patch_host_player();
    if (fix_member_clear()) lg("engine fix: online user kept out of the casino member clear");
    if (!hook_client_join()) lg("engine differs; going back into a match is not safe");
    if (!hook_key_alloc() || !hook_ctrl_command()) { rejoin_off = 1; lg("engine differs; players who lose the connection are not held"); }
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
 * length selects the mission and the player count; the values are not used). */
static void *build_host_screen(int players) {
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
    lg("host screen %p for %d players, host hero %d", screen, players, (int)host_hero);
    return screen;
}

/* ------------------------------------------------------------------ JNI (rg.HostServer) */
static int hosting;
static volatile int picking;             /* the host's hero select is up */
static DWORD last_ping, retry;

/* The LAN host screen is up: offer the room on the relay (pump connects the first time). */
static void start_serving(void) {
    for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;    /* a previous game's players */
    if (hosting && ws_up) {      /* hosting again: offer the room on the open relay connection */
        char json[300];
        snprintf(json, sizeof json, "{\"t\":\"serve\",\"room\":\"%s\",\"players\":%d}", room, remote_players);
        ws_send(1, json, strlen(json));
        lg("hosting room %s again", room);
    }
    hosting = 1;
    serving = 1;
}

/* The host picks a hero in the engine's own hero select (HeroSelectionScreen, ctor 0x100dcff0,
 * the one Skirmish uses: (ctx, std::string *out, bool popOnSelect), 0xb8 bytes). The instance
 * gets a private copy of its 13-slot vtable (0x101888e0) whose onSelect (slot 11, 0x100dc7e0) and
 * onBack (slot 12, 0x100dbfc0) run here; the shared code, Skirmish and the arena stay untouched. */
typedef void (TC *hs_ctor_fn)(void *self, uint32_t ctx, void *out, int pop_on_select);
typedef void (TC *this_fn)(void *self);
typedef void (TC *this_int_fn)(void *self, int v);
typedef void (TC *str_ctor_fn)(void *s, const char *c, int enc);
static void *hs_vt[13], *hs_str;
static volatile int hs_done, pick_players;

static void TC hs_on_select(void *self) {
    if (hs_done) return;
    hs_done = 1;
    ((this_fn)P(0x100dc7e0))(self);            /* resolves Random; popOnSelect = 0, so no pop */
    int hero = *(int *)((uint8_t *)self + 0x68);
    host_hero = (hero >= 0 && hero <= 9) ? (uint32_t)hero : 0xffffffffu;
    picking = 0;
    void *lan = build_host_screen(pick_players);
    ((set_screen_fn)P(0x1008f0e0))(self, lan);  /* Screen::replaceTop: the select goes, the LAN host screen comes */
    start_serving();
}

static void TC hs_on_back(void *self) {
    if (hs_done) return;
    hs_done = 1;
    picking = 0;
    lg("hosting cancelled");
    ((this_fn)P(0x100dbfc0))(self);            /* back to the menu */
}

static int show_hero_select(int players) {
    if (!hs_str) { hs_str = eng_new(4); ((str_ctor_fn)P(0x100f0e10))(hs_str, "", 0); }   /* ours; the screen never frees it */
    uint32_t ctx = LD32(menu_base + 0x10);
    void *hs = eng_new(0xb8);
    ((hs_ctor_fn)P(0x100dcff0))(hs, ctx, hs_str, 0);
    ((this_fn)P(0x100dd5b0))(hs);              /* the ten heroes, Plumber first */
    ((this_int_fn)P(0x100dd340))(hs, -2);      /* and Random */
    memcpy(hs_vt, (void *)(uintptr_t)A(0x101888e0), sizeof hs_vt);   /* already relocated */
    hs_vt[11] = (void *)hs_on_select;
    hs_vt[12] = (void *)hs_on_back;
    *(void ***)hs = hs_vt;
    hs_done = 0;
    pick_players = players;
    picking = 1;
    ((set_screen_fn)(uintptr_t)LD32(LD32(menu_base) + 0x14))((void *)(uintptr_t)menu_base, hs);
    lg("hero select %p", hs);
    return 1;
}

/* A match is running while a remote player's connection is alive. */
static int match_live(void) {
    for (int i = 0; i < nsess; i++) if (sess[i].used && sess[i].accepted && !sess[i].closed) return 1;
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
    for (int i = 0; i < nsess; i++) if (sess[i].used && !sess[i].closed) n++;
    return n;
}

/* 0 = not hosting, 1 = the host is choosing a hero, 2 = serving (waiting for players or playing) */
JNIEXPORT jint JNICALL Java_rg_HostServer_state(JNIEnv *env, jclass c) { (void)env; (void)c; return picking ? 1 : serving ? 2 : 0; }

JNIEXPORT jboolean JNICALL Java_rg_HostServer_menuReady(JNIEnv *env, jclass c) { (void)env; (void)c; return menu_base != 0; }

/* The engine opens its files with the ANSI API (CreateFileA etc.) under the home directory that
 * Java passes it as UTF-8, so a non-ASCII game folder (C:\Users\Hlavatý\...) cannot be opened.
 * HostServer.engineHome() then makes that folder the process's current directory (wide API) and
 * gives the engine a relative home. */
JNIEXPORT jboolean JNICALL Java_rg_HostServer_chdir(JNIEnv *env, jclass c, jstring jdir) {
    (void)c;
    jsize n = (*env)->GetStringLength(env, jdir);
    WCHAR *w = calloc((size_t)n + 1, sizeof(WCHAR));
    if (!w) return JNI_FALSE;
    (*env)->GetStringRegion(env, jdir, 0, n, (jchar *)w);
    BOOL ok = SetCurrentDirectoryW(w);
    free(w);
    return ok ? JNI_TRUE : JNI_FALSE;
}

/* hero: 0..9 = that hero, -1 = random, -2 = the host chooses in the hero select */
JNIEXPORT jboolean JNICALL Java_rg_HostServer_host(JNIEnv *env, jclass c, jstring jurl, jstring jroom, jint players,
                                                   jstring jname, jint hero) {
    (void)c;
    if (installed <= 0 || match_live() || picking || serving) return JNI_FALSE;
    if (!menu_base) { lg("menu not ready yet"); return JNI_FALSE; }
    const char *u = (*env)->GetStringUTFChars(env, jurl, NULL), *r = (*env)->GetStringUTFChars(env, jroom, NULL);
    const char *n = (*env)->GetStringUTFChars(env, jname, NULL);
    snprintf(url, sizeof url, "%s", u);
    snprintf(room, sizeof room, "%s", r);
    snprintf(host_name, sizeof host_name, "%s", n);
    (*env)->ReleaseStringUTFChars(env, jurl, u);
    (*env)->ReleaseStringUTFChars(env, jroom, r);
    (*env)->ReleaseStringUTFChars(env, jname, n);
    if (players < 2) players = 2;
    if (players > 4) players = 4;
    remote_players = players - 1;
    if (hero == -2) return (jboolean)show_hero_select(players);
    host_hero = (hero >= 0 && hero <= 9) ? (uint32_t)hero : 0xffffffffu;
    void *screen = build_host_screen(players);
    ((set_screen_fn)(uintptr_t)LD32(LD32(menu_base) + 0x14))((void *)(uintptr_t)menu_base, screen);
    start_serving();
    return JNI_TRUE;
}

JNIEXPORT void JNICALL Java_rg_HostServer_pump(JNIEnv *env, jclass c) {
    (void)env; (void)c;
    if (!hosting) return;
    DWORD now = GetTickCount();
    if (!ws_up && ws == INVALID_SOCKET && (now - retry > 2000 || !retry)) { retry = now ? now : 1; ws_connect(); }
    if (ws_up && now - last_ping > 15000) { last_ping = now; ws_send(1, "{\"t\":\"ping\"}", 12); }
    track_seats();
    ev *e;
    while ((e = pop_ev())) {
        sess_t *s = by_pid(e->pid);
        if (e->kind == EV_JOIN) {
            if (s && s->closed) s = NULL;      /* the same relay pid in a new game */
            if (!s) {
                if (nsess < MAX_SESS) s = &sess[nsess++];
                else lg("FATAL: no player slot left");
                if (s) {
                    memset(s, 0, sizeof *s);
                    s->used = 1;
                    s->pid = e->pid;
                    if (e->len > 0) snprintf(s->name, sizeof s->name, "%.*s", e->len, (const char *)e->data);
                    lg("player %d (%s) joined", e->pid, s->name);
                }
            }
            free(e);
        } else if (e->kind == EV_LEAVE || e->kind == EV_LOST) {
            if (s && e->kind == EV_LOST && serving && !rejoin_off && s->key > 0 && !s->closed) {
                s->away = 1;            /* the seat is held: see track_seats */
                s->away_since = GetTickCount();
                free_queue(s);
                lg("player %d (%s) lost the connection; seat %d held", e->pid, s->name, s->slot);
            } else if (s) {
                s->closed = 1;
                lg("player %d left", e->pid);
            }
            free(e);
        } else if (e->kind == EV_DATA && s) {
            e->next = NULL;
            if (s->in_tail) s->in_tail->next = e; else s->in_head = e;
            s->in_tail = e;
        } else if (e->kind == EV_CLOSED) {
            lg("relay connection lost");
            if (ws != INVALID_SOCKET) { closesocket(ws); ws = INVALID_SOCKET; }
            for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;
            free(e);
        } else free(e);
    }
}
