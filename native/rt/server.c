/* Hosting a game on this player's engine (every native build), and the engine fixes that go with
 * it. See docs/MULTIPLAYER.md for how and why each piece works (it began as rghost.c, the retired
 * Java build's host DLL; see the git history). In short:
 *   - the engine's LAN host screen (0x100caa80) is built and made the current screen; its entry
 *     creates the server-mode controller (0x1000ecd0) and a TCP acceptor;
 *   - the acceptor's setController (vtable 0x10188b64, slot 1) is replaced by a stub that installs
 *     a fake listener (guest vtable, host stubs) whose accept() hands out "HostConn" objects;
 *   - those connections carry channel 0x16 through the relay: the room's GameRoom bot forwards
 *     each remote player's messages to us as [u16 pid][zlib message] and back;
 *   - a player whose connection drops keeps their seat; coming back (same name, Quick Match) they
 *     get their old key and seat (track_seats, key_alloc, ctrl_command, client_join).
 * The host's engine is the game server and player 0, like the original LAN mode.
 *
 * F5 (rg_key) or rg_host_request() opens a player-count picker drawn over the game
 * (rg_server_compose), then the engine's own hero select, then the LAN host screen. Everything
 * engine-facing runs on the game thread (rg_server_pump before each tick, and the stubs and hooks
 * the engine calls); the relay socket has a thread that only queues events (in the browser build
 * the same work is polled from rg_server_pump).
 *
 * Test mode (rgtest --server URL ROOM): hosts by itself once the menu is up. */
#include "server.h"
#include "engine.h"
#include "host.h"
#include "link.h"
#include "net.h"
#include "rt.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>


#define MAX_SESS 4096              /* slots are never reused: the engine never frees a connection */
#define CH_GAME 0x16
#define HOST_AFTER_FRAMES 300      /* test mode: the intro is skipped by then (keys) */
#define HOLD_MS 0                  /* how long a dropped player's seat is held, ms (0: until the match ends) */
#define AWT_F5 116

static int test_mode, test_players = 2;
static uint32_t menu_base;               /* the main menu flow object (constructor 0x1008f500) */
static char url[256], room[72], test_room[72];
static int remote_players, games;
static int hosting;                      /* hosted at least once: the relay link stays up */
static volatile int serving;             /* until the engine closes our listener (l_close) */
static int picking;                      /* the host's hero select is up */
static int picker, picker_default = 2, hero_default = -2, swallow_key, match_started;
static uint32_t host_hero = 0xffffffffu; /* 0..9 = that hero, anything else = random */
static uint32_t host_name;               /* guest string: the host's nickname */
static char status[160];

static uint32_t eng_new(uint32_t n) {
    uint32_t a[1] = {n}, p = rt_guest_call(0x1014f476, 1, a);   /* the engine's operator new */
    memset(M + p, 0, n);
    return p;
}
static uint32_t tcall(uint32_t fn, uint32_t self, int n, uint32_t a0, uint32_t a1, uint32_t a2) {
    uint32_t a[3] = {a0, a1, a2};
    return rt_guest_thiscall(fn, self, n, a);
}

/* ------------------------------------------------------------------ relay link (WebSocket) */
typedef struct ev { struct ev *next; int kind, pid, len; uint8_t data[]; } ev;
enum { EV_JOIN, EV_LEAVE, EV_LOST, EV_DATA, EV_CLOSED };

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER, send_lock = PTHREAD_MUTEX_INITIALIZER;
static ev *ev_head, *ev_tail;
static rt_link *ws;                      /* the relay connection (guarded by send_lock) */
static volatile int ws_up, connecting, no_relay;

static void push_ev(int kind, int pid, const void *data, int len) {
    ev *e = (ev *)calloc(1, sizeof *e + (size_t)(len > 0 ? len : 0));
    e->kind = kind;
    e->pid = pid;
    e->len = len;
    if (len > 0) memcpy(e->data, data, (size_t)len);
    pthread_mutex_lock(&lock);
    if (ev_tail) ev_tail->next = e; else ev_head = e;
    ev_tail = e;
    pthread_mutex_unlock(&lock);
}
static ev *pop_ev(void) {
    pthread_mutex_lock(&lock);
    ev *e = ev_head;
    if (e) { ev_head = e->next; if (!ev_head) ev_tail = NULL; }
    pthread_mutex_unlock(&lock);
    return e;
}

static int ws_send(int opcode, const void *data, size_t len) {
    pthread_mutex_lock(&send_lock);
    rt_link *l = ws_up ? ws : NULL;
    if (l) link_ref(l);
    pthread_mutex_unlock(&send_lock);
    int ok = l && link_send(l, opcode, data, len);
    link_release(l);
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

static void send_serve(void) {
    char json[200];
    snprintf(json, sizeof json, "{\"t\":\"serve\",\"room\":\"%s\",\"players\":%d}", room, remote_players);
    ws_send(1, json, strlen(json));
    host_log("host: offering room %s on %s for %d remote player(s)", room, url, remote_players);
}

/* a frame from the relay (op 1 text, 2 binary; msg NUL-terminated) */
static void on_relay_frame(int op, const uint8_t *msg, size_t len) {
    if (op == 1) {
        char t[24];
        json_str((const char *)msg, "t", t, sizeof t);
        if (!strcmp(t, "player_joined")) {
            char name[32];
            json_str((const char *)msg, "name", name, sizeof name);
            push_ev(EV_JOIN, json_int((const char *)msg, "pid", -1), name, (int)strlen(name) + 1);
        } else if (!strcmp(t, "player_left")) {
            push_ev(json_int((const char *)msg, "lost", 0) ? EV_LOST : EV_LEAVE, json_int((const char *)msg, "pid", -1), NULL, 0);
        } else if (!strcmp(t, "error")) host_log("host: relay error %s", (const char *)msg);
        else if (getenv("RT_NET_DEBUG")) host_log("host: relay %s", (const char *)msg);
    } else if (len >= 2) {
        push_ev(EV_DATA, msg[0] | msg[1] << 8, msg + 2, (int)len - 2);
    }
}

/* the link is open: it becomes `ws` (with a reference of its own) and we introduce ourselves */
static void link_up(rt_link *l) {
    link_ref(l);
    pthread_mutex_lock(&send_lock);
    ws = l;
    ws_up = 1;
    pthread_mutex_unlock(&send_lock);
    connecting = 0;
    ws_send(1, "{\"t\":\"hello\",\"v\":1,\"name\":\"RGHost\"}", 35);
    if (serving) send_serve();
}

/* the link ended: the reader's reference goes, and the game thread closes `ws` (EV_CLOSED) */
static void link_down(rt_link *l) {
    link_release(l);
    pthread_mutex_lock(&send_lock);
    ws_up = 0;
    pthread_mutex_unlock(&send_lock);
    push_ev(EV_CLOSED, 0, NULL, 0);
}

static rt_link *open_relay(void) {
    char buf[128];
    const char *u = relay_for_hosting(net_server_url(), net_nickname(), buf, sizeof buf);
    no_relay = !u;
    rt_link *l = u ? link_open(u, "host") : NULL;
    if (l) snprintf(url, sizeof url, "%s", u);
    return l;
}

#ifndef RT_NO_THREADS
/* connects (off the game thread: a dead server must not freeze the game), then reads */
static void *link_thread(void *arg) {
    (void)arg;
    rt_link *l = open_relay();
    if (!l) { connecting = 0; return NULL; }
    link_up(l);
    uint8_t *msg = (uint8_t *)malloc(1 << 20);
    size_t len;
    int op;
    while ((op = link_read(l, msg, 1 << 20, &len)) != 0) on_relay_frame(op, msg, len);
    free(msg);
    link_down(l);
    return NULL;
}
#else
/* Without threads (the browser build): the same, stepped from rg_server_pump. */
static rt_link *pending;                 /* the link being opened or read (the reader's reference) */
static uint32_t pending_since;

static void link_start(void) {
    pending = open_relay();
    pending_since = host_time_ms();
    if (!pending) connecting = 0;
}

static void link_step(void) {
    if (!pending) return;
    if (!ws_up) {                        /* still connecting */
        int s = link_state(pending);
        if (s == 1) link_up(pending);
        else if (s < 0 || host_time_ms() - pending_since > 10000) {
            link_close(pending);
            link_release(pending);
            pending = NULL;
            connecting = 0;
        }
        return;
    }
    static uint8_t *msg;
    if (!msg) msg = (uint8_t *)malloc(1 << 20);
    for (;;) {
        size_t len;
        int op = link_poll(pending, msg, 1 << 20, &len);
        if (op == 0) return;
        if (op < 0) { link_down(pending); pending = NULL; return; }
        on_relay_frame(op, msg, len);
    }
}
#endif

/* ------------------------------------------------------------------ sessions */
typedef struct {
    int used, pid, accepted, closed;
    uint32_t obj, addr;                  /* guest connection and address objects */
    ev *in_head, *in_tail;
    char name[32];                       /* relay nickname */
    int key, slot;                       /* the engine's player key and seat (0 = not yet known) */
    int checked;                         /* rejoin: looked for a held seat */
    int away;                            /* the relay connection dropped; the engine is not told (seat held) */
    uint32_t away_since;
    int heir_of;                         /* rejoin: index + 1 of the held session this one takes over */
} sess_t;
static sess_t sess[MAX_SESS];
static int nsess;                        /* slots handed out so far */
static uint32_t srv_ctrl;                /* the engine's server controller (pending joins at +0x94) */
static int force_key, drop_join_key, rejoin_off;

static sess_t *by_pid(int pid) {
    sess_t *r = NULL;                    /* the newest session with this pid */
    for (int i = 0; i < nsess; i++) if (sess[i].used && sess[i].pid == pid) r = &sess[i];
    return r;
}
static sess_t *sess_of(uint32_t obj) { uint32_t i = LD32(obj + 4); return i < (uint32_t)nsess ? &sess[i] : NULL; }

static void to_player(sess_t *s, const uint8_t *data, uint32_t len) {
    uint8_t *buf = (uint8_t *)malloc((size_t)len + 2);
    buf[0] = (uint8_t)s->pid;
    buf[1] = (uint8_t)(s->pid >> 8);
    memcpy(buf + 2, data, len);
    ws_send(2, buf, (size_t)len + 2);
    free(buf);
}

static void free_queue(sess_t *s) {
    while (s->in_head) { ev *n = s->in_head->next; free(s->in_head); s->in_head = n; }
    s->in_tail = NULL;
}

/* the pending join of a session, or 0 (ctrl+0x94: list of {prev, next, entry}; entry = {conn
 * wrapper (our object at +0x7c), state, info, key +0xc, seat +0x10}) */
static uint32_t pending_entry(sess_t *s) {
    if (!srv_ctrl || !s->obj) return 0;
    for (uint32_t node = LD32(srv_ctrl + 0x94); node; node = LD32(node + 4)) {
        uint32_t ent = LD32(node + 8);
        if (ent && LD32(ent) && LD32(LD32(ent) + 0x7c) == s->obj) return ent;
    }
    return 0;
}

/* The new connection has its battle state: the held session now belongs to it. */
static void take_over(sess_t *s) {
    sess_t *old = &sess[s->heir_of - 1];
    s->heir_of = 0;
    host_log("host: player %d (%s) is back in the match (was player %d)", s->pid, s->name, old->pid);
    free_queue(old);
    old->pid = s->pid;
    old->away = 0;
    old->in_head = s->in_head;           /* anything the new connection sent after its handshake */
    old->in_tail = s->in_tail;
    s->in_head = s->in_tail = NULL;
    s->used = 0;                         /* by_pid finds the held session now */
}

static void track_seats(void) {
    uint32_t now = host_time_ms();
    for (int i = 0; i < nsess; i++) {
        sess_t *s = &sess[i];
        if (HOLD_MS && s->used && s->away && now - s->away_since > HOLD_MS) {
            s->away = 0;
            s->closed = 1;
            host_log("host: player %d (%s) did not come back; the engine is told", s->pid, s->name);
        }
        if (!s->used || s->closed || s->key || !s->accepted) continue;
        uint32_t ent = pending_entry(s);
        if (!ent) continue;
        uint32_t st = LD32(ent + 4), key = LD32(ent + 0xc), slot = LD32(ent + 0x10);
        if (st == 3 && slot == 0 && !s->checked && s->name[0]) {
            s->checked = 1;
            for (int j = 0; j < nsess; j++) {
                sess_t *o = &sess[j];
                if (o != s && o->used && o->away && o->key > 0 && o->slot > 0 && !strcasecmp(o->name, s->name)) {
                    o->away_since = now;         /* do not give the seat up during the handshake */
                    s->heir_of = j + 1;
                    ST32(ent + 0x10, (uint32_t)o->slot);
                    force_key = o->key;
                    drop_join_key = o->key;
                    host_log("host: player %d (%s) comes back: key %d, seat %d", s->pid, s->name, o->key, o->slot);
                    break;
                }
            }
        }
        if (st == 4 && key) {
            s->key = (int)key;
            s->slot = (int)slot;
            host_log("host: player %d (%s) is key %u, seat %u", s->pid, s->name, key, slot);
        }
    }
}

static int remote_connected(void) {
    int n = 0;
    for (int i = 0; i < nsess; i++) if (sess[i].used && !sess[i].closed) n++;
    return n;
}
/* a match is running while a remote player's connection is alive */
static int match_live(void) {
    for (int i = 0; i < nsess; i++) if (sess[i].used && sess[i].accepted && !sess[i].closed) return 1;
    return 0;
}

/* ------------------------------------------------------------------ guest objects (MT transport) */
static uint32_t listener_obj, conn_vt, addr_vt, hs_vt, hs_str;

static void bad_slot(int slot) {
    rt_fatal("host: unimplemented MT object method (vtable slot 0x%x, this=%08x, ret=%08x)", slot, cpu.r[ECX], LD32(cpu.r[ESP]));
}
static uint32_t make_vtable(const char *name, int slots) {
    uint32_t vt = rt_heap_alloc((uint32_t)slots * 4);
    for (int i = 0; i < slots; i++) {
        char *n = (char *)malloc(64);
        snprintf(n, 64, "%s+0x%x", name, i * 4);
        ST32(vt + (uint32_t)i * 4, rt_stub_register(n, bad_slot, i * 4));
    }
    return vt;
}

/* acceptor (0x100df500): setController(controller) -> our listener instead of tcp://:12345 */
static void acc_set_controller(int arg) {
    (void)arg;
    uint32_t acc = cpu.r[ECX], ctrl = rt_arg(0);
    ST32(acc + 4, ctrl);
    ST32(acc + 8, listener_obj);
    ST8(acc + 0x18, 1);
    srv_ctrl = ctrl;
    host_log("host: controller %08x attached to the acceptor %08x", ctrl, acc);
    rt_ret(1, 1);
}
static void any_dtor(int arg) { (void)arg; rt_ret(cpu.r[ECX], 1); }
static void l_accept(int arg) {
    (void)arg;
    for (int i = 0; i < nsess; i++) {
        sess_t *s = &sess[i];
        if (s->used && !s->accepted) {
            s->accepted = 1;
            s->obj = rt_heap_alloc(0x400);
            memset(M + s->obj, 0, 0x400);
            ST32(s->obj, conn_vt);
            ST32(s->obj + 4, (uint32_t)i);
            host_log("host: accepted player %d (conn %08x)", s->pid, s->obj);
            rt_ret(s->obj, 0);
            return;
        }
    }
    rt_ret(0, 0);
}
/* The engine's game server is gone (match over, or the host left the host screen): the relay must
 * stop sending Quick Match players to this room. */
static void l_close(int arg) {
    (void)arg;
    host_log("host: listener closed");
    serving = 0;
    for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;   /* that game's players */
    ws_send(1, "{\"t\":\"unserve\"}", 15);
    rt_ret(0, 0);
}

/* conn.address(): callers only print it (vtable slot 0x14 no-op; string object at +0xc -> header
 * -> char* at +0xc) */
static void addr_nop(int arg) { (void)arg; rt_ret(0, 1); }
static void c_address(int arg) {
    (void)arg;
    sess_t *s = sess_of(cpu.r[ECX]);
    if (s && !s->addr) {
        char name[32];
        snprintf(name, sizeof name, "relay:player%d", s->pid);
        s->addr = rt_heap_alloc(0x20);
        uint32_t hdr = rt_heap_alloc(0x20);
        memset(M + s->addr, 0, 0x20);
        memset(M + hdr, 0, 0x20);
        ST32(hdr + 0xc, rt_strdup_guest(name));
        ST32(s->addr, addr_vt);
        ST32(s->addr + 0xc, hdr);
    }
    rt_ret(s ? s->addr : 0, 0);
}
static void c_dtor(int arg) { (void)arg; rt_ret(cpu.r[ECX], 1); }
static void c_state(int arg) { (void)arg; rt_ret(3, 0); }
static void c_alive(int arg) { (void)arg; sess_t *s = sess_of(cpu.r[ECX]); rt_ret(s && !s->closed, 0); }
static void c_close(int arg) {
    (void)arg;
    sess_t *s = sess_of(cpu.r[ECX]);
    if (s) { host_log("host: the engine closes the connection of player %d", s->pid); s->closed = 1; }
    rt_ret(0, 0);
}
static void c_send(int arg) {
    (void)arg;
    sess_t *s = sess_of(cpu.r[ECX]);
    uint32_t ch = rt_arg(0), ptr = rt_arg(1), len = rt_arg(2);
    /* not to a closed session (its relay pid may belong to a player who has come back since), nor
     * to one whose player is away (see track_seats) */
    if (s && !s->closed && !s->away && ch == CH_GAME && len <= 1u << 20) {
        if (getenv("RT_NET_DEBUG")) host_log("host: -> player %d: %u bytes", s->pid, len);
        to_player(s, M + ptr, len);
        /* handshake state 4 sends the battle state, then gives up registering a key it has */
        if (s->heir_of) { uint32_t ent = pending_entry(s); if (ent && LD32(ent + 4) == 4) take_over(s); }
    }
    rt_ret(1, 3);
}
/* recv(channel, flag) -> MT message object (ByteStream at +0xc) or 0 */
static void c_recv(int arg) {
    (void)arg;
    uint32_t obj = cpu.r[ECX], ch = rt_arg(0);
    sess_t *s = sess_of(obj);
    if (!s || ch != CH_GAME || !s->in_head) { rt_ret(0, 2); return; }
    ev *e = s->in_head;
    s->in_head = e->next;
    if (!s->in_head) s->in_tail = NULL;
    /* never freed by us: the engine may keep messages (the real connection pools them) */
    uint32_t msg = eng_new(0x48);
    tcall(0x1011f700, msg, 1, (uint32_t)e->len + 0x40, 0, 0);       /* MT message constructor */
    uint32_t buf = rt_heap_alloc((uint32_t)e->len + 1);
    memcpy(M + buf, e->data, (size_t)e->len);
    uint32_t os = msg + 0xc + 8;                                    /* ostream part: write(buf, len) */
    tcall(LD32(LD32(os)), os, 2, buf, (uint32_t)e->len, 0);
    rt_heap_free(buf);
    if (getenv("RT_NET_DEBUG")) host_log("host: <- player %d: %d bytes", s->pid, e->len);
    free(e);
    rt_ret(msg, 2);
}

/* ------------------------------------------------------------------ rejoin */
/* 0x10023f40 key allocation (entry hook): a player coming back gets their old key */
static int hook_key_alloc(void) {
    if (force_key <= 0) return 0;
    uint32_t pool = cpu.r[ECX], used = LD32(pool + 0x1c);
    int k = force_key;
    force_key = 0;
    ST8(used + (uint32_t)k, 1);
    cpu.r[EAX] = (uint32_t)k;
    cpu.r[ECX] = used;
    cpu.r[ESP] += 4;                                                /* ret */
    return 1;
}
/* the server controller's command dispatch (vtable 0x101779a0 slot 9, 0x1000e150): the join
 * command for a player coming back goes only to them */
static void ctrl_command(int arg) {
    (void)arg;
    uint32_t ctrl = cpu.r[ECX], c = rt_arg(0);
    if (drop_join_key > 0 && c && tcall(0x1000ce50, c, 0, 0, 0, 0) == 0xc && (int)LD32(c + 0x44) == drop_join_key) {
        static uint32_t only;
        if (!only) only = rt_heap_alloc(4);
        uint32_t keys = LD32(c + 0x24), n = LD32(c + 0x2c);         /* recipients (player keys) */
        ST32(only, (uint32_t)drop_join_key);
        ST32(c + 0x24, only);
        ST32(c + 0x2c, 1);
        tcall(0x1005c1d0, LD32(ctrl + 0x8c), 1, c, 0, 0);            /* broadcast to the sessions */
        ST32(c + 0x24, keys);
        ST32(c + 0x2c, n);
        host_log("host: join command for key %d sent only to that player (coming back; %u recipients)", drop_join_key, n);
        drop_join_key = 0;
        tcall(0x100f3e70, c, 1, 0, 0, 0);                          /* unref */
        if (!tcall(0x10112570, c, 0, 0, 0, 0)) tcall(LD32(LD32(c)), c, 1, 1, 0, 0);   /* delete */
    } else {
        tcall(0x1000e150, ctrl, 1, c, 0, 0);
    }
    rt_ret(0, 1);
}
/* The client controller's join handler (vtable 0x10188d18 slot 0x58, 0x100e16a0), every instance:
 * for a key and seat already in the game, put the old player objects back after it. */
static uint32_t join_rec(uint32_t g, int key) {
    uint32_t recs = LD32(g + 0x1c);
    return recs && key > 0 && key < (int)LD32(recs + 0xc) && LD32(recs + 0x10) ? LD32(recs + 0x10) + 4u * (uint32_t)key : 0;
}
static uint32_t join_seat(uint32_t g, int seat) {
    uint32_t seats = LD32(g + 0x14);
    return seats && seat > 0 && seat <= 4 && LD32(seats + 0x2c) ? LD32(seats + 0x2c) + 4u * (uint32_t)seat : 0;
}
static void client_join(int arg) {
    (void)arg;
    uint32_t g = cpu.r[ECX], c = rt_arg(0);
    int key = c ? (int)LD32(c + 0x44) : 0, seat = c ? (int)LD32(c + 0x48) : 0;
    uint32_t r = join_rec(g, key), p = join_seat(g, seat);
    uint32_t old_rec = r ? LD32(r) : 0, old_player = p ? LD32(p) : 0;
    tcall(0x100e16a0, g, 1, c, 0, 0);                              /* releases cmd */
    if (old_rec && old_player && (r = join_rec(g, key)) && (p = join_seat(g, seat))) {
        ST32(r, old_rec);
        ST32(p, old_player);
        host_log("host: back in a match: key %d, seat %d kept", key, seat);
    }
    rt_ret(0, 1);
}

/* ------------------------------------------------------------------ engine fixes (every instance) */
static void save_regs(uint32_t *r) { memcpy(r, cpu.r, sizeof cpu.r); }
static void load_regs(const uint32_t *r) { memcpy(cpu.r, r, sizeof cpu.r); }

/* PlaceTileUIState::enter (0x100b2c20), before `cmp eax,5` at 0x100b2c66: for a player who has
 * surrendered or been knocked out (state 5, or no player object) it returns early without setting
 * the tile layer (this+0x84 = this->layer(2), 0x1008cbc0), which update still writes into: a
 * crash or heap corruption for the last player to surrender in a 3-4 player match. */
static int hook_place_tile(void) {
    uint32_t self = cpu.r[ESI], st = cpu.r[EAX];
    if (st != 5 && LD32(LD32(LD32(self + 0x10) + 0x2c) + st * 4)) return 0;
    uint32_t r[8];
    save_regs(r);
    uint32_t layer = tcall(0x1008cbc0, self, 1, 2, 0, 0);
    load_regs(r);
    ST32(self + 0x84, layer);
    return 0;
}
/* Casino member clear (0x10094eac: mov edx,[ecx]; push 1; call [edx]): never delete the context's
 * own online user object (context 0x101b828c, user at +0xe4), which a stale member entry may hold. */
static int hook_member_clear(void) {
    uint32_t ctx = LD32(0x101b828c);
    return ctx && cpu.r[ECX] == LD32(ctx + 0xe4);
}

/* ------------------------------------------------------------------ the host's hero and name */
/* LAN host entry (0x100ca920): at 0x100ca9d9 (mov ebp,edx) EDX is the host's random hero */
static int hook_host_hero(void) {
    if (host_hero <= 9) cpu.r[EDX] = host_hero;
    return 0;
}
/* at 0x100caa30 EDI is the host's player setup (+0 name string): give it the nickname */
static int hook_host_name(void) {
    if (!host_name || !cpu.r[EDI]) return 0;
    uint32_t r[8];
    save_regs(r);
    tcall(0x100f10b0, cpu.r[EDI], 1, host_name, 0, 0);               /* std::string assign */
    load_regs(r);
    return 0;
}

/* ------------------------------------------------------------------ hosting */
static void set_screen(uint32_t screen) { tcall(LD32(LD32(menu_base) + 0x14), menu_base, 1, screen, 0, 0); }

/* What the menus would do for "host a LAN game": build the LAN host screen (a hero list whose
 * length selects the mission and the player count). */
static uint32_t build_host_screen(int players) {
    uint32_t ctx = LD32(menu_base + 0x10), list = eng_new(0xc);
    for (int i = 0; i < players; i++) {
        uint32_t val = eng_new(4);
        ST32(val, (uint32_t)i);
        tcall(0x10104a40, list, 1, val, 0, 0);                      /* list.push_back(&hero) */
    }
    uint32_t screen = eng_new(0x3c);
    tcall(0x100caa80, screen, 2, ctx, list, 0);
    host_log("host: host screen %08x for %d players, host hero %d", screen, players, (int)host_hero);
    return screen;
}

static void start_serving(void) {
    for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;    /* a previous game's players */
    serving = 1;
    match_started = 0;
    if (hosting && ws_up) send_serve();   /* hosting again on the open relay connection */
    hosting = 1;
}

/* The host picks a hero in the engine's own hero select (HeroSelectionScreen, ctor 0x100dcff0:
 * (ctx, std::string *out, bool popOnSelect), 0xb8 bytes) with a private copy of its 13-slot vtable
 * (0x101888e0): onSelect (slot 11, 0x100dc7e0) and onBack (slot 12, 0x100dbfc0) run here. */
static int hs_done, pick_players;
static void hs_on_select(int arg) {
    (void)arg;
    uint32_t self = cpu.r[ECX];
    if (!hs_done) {
        hs_done = 1;
        tcall(0x100dc7e0, self, 0, 0, 0, 0);                       /* resolves Random; no pop */
        int hero = (int)LD32(self + 0x68);
        host_hero = (hero >= 0 && hero <= 9) ? (uint32_t)hero : 0xffffffffu;
        picking = 0;
        uint32_t lan = build_host_screen(pick_players);
        tcall(0x1008f0e0, self, 1, lan, 0, 0);                     /* Screen::replaceTop */
        start_serving();
    }
    rt_ret(0, 0);
}
static void hs_on_back(int arg) {
    (void)arg;
    uint32_t self = cpu.r[ECX];
    if (!hs_done) {
        hs_done = 1;
        picking = 0;
        host_log("host: hosting cancelled");
        tcall(0x100dbfc0, self, 0, 0, 0, 0);                       /* back to the menu */
    }
    rt_ret(0, 0);
}
static void show_hero_select(int players) {
    if (!hs_str) { hs_str = eng_new(4); tcall(0x100f0e10, hs_str, 2, rt_strdup_guest(""), 0, 0); }   /* never freed by the screen */
    uint32_t hs = eng_new(0xb8);
    tcall(0x100dcff0, hs, 3, LD32(menu_base + 0x10), hs_str, 0);
    tcall(0x100dd5b0, hs, 0, 0, 0, 0);                              /* the ten heroes */
    tcall(0x100dd340, hs, 1, (uint32_t)-2, 0, 0);                   /* and Random */
    ST32(hs, hs_vt);
    hs_done = 0;
    pick_players = players;
    picking = 1;
    set_screen(hs);
    host_log("host: hero select %08x", hs);
}

/* hero: 0..9 = that hero, -1 = random, -2 = the host chooses in the hero select */
static int host_start(int players, int hero, const char *room_name) {
    if (!menu_base || match_live() || picking || serving) {
        host_log("host: cannot host now (not in the main menu yet, or already hosting)");
        return 0;
    }
    const char *nick = net_nickname();
    if (room_name) snprintf(room, sizeof room, "%s", room_name);
    else snprintf(room, sizeof room, "HOST_%s_%d", nick, ++games);
    host_name = rt_strdup_guest(nick);
    players = players < 2 ? 2 : players > 4 ? 4 : players;
    remote_players = players - 1;
    host_log("host: hosting %s for %d players (server=%s)", room, players, net_server_url());
    if (hero == -2) { show_hero_select(players); return 1; }
    host_hero = (hero >= 0 && hero <= 9) ? (uint32_t)hero : 0xffffffffu;
    set_screen(build_host_screen(players));
    start_serving();
    return 1;
}

/* ------------------------------------------------------------------ menu hooks */
static int hook_menu_ctor(void) { menu_base = cpu.r[ECX]; return 0; }
static const char *ev_name(uint32_t ev) { uint32_t hdr = LD32(ev + 4); return (const char *)(M + LD32(hdr + 12)); }
static int hook_menu(void) {
    if (getenv("RT_NET_DEBUG")) {
        uint32_t ev = LD32(cpu.r[ESP] + 4);
        host_log("host: menu event type %u \"%s\" (this=%08x)", LD32(ev), ev_name(ev), cpu.r[ECX]);
    }
    return 0;
}

/* ------------------------------------------------------------------ API */
void rg_server_enable(const char *u, const char *r) {
    test_mode = 1;
    rg_net_configure(u, NULL);
    snprintf(test_room, sizeof test_room, "%s", r);
}
int rg_server_enabled(void) { return test_mode; }

void rg_host_configure(int players, int hero) {
    picker_default = players < 2 ? 2 : players > 4 ? 4 : players;
    hero_default = hero < -2 || hero > 9 ? -2 : hero;
}

void rg_host_request(void) {
    if (picker || picking || serving || match_live()) { host_log("host: already hosting"); return; }
    if (!menu_base) { host_log("host: the main menu is not up yet"); return; }
    picker = picker_default;
    host_log("host: choosing the number of players");
}

int rg_host_state(void) {
    if (picker) return 1;
    if (picking) return 2;
    if (serving) return match_started ? 4 : 3;
    return 0;
}

int rg_host_players(void) { return picker_default; }

int rg_host_start(int players, int hero) { return !picker && host_start(players, hero, NULL); }

const char *rg_host_status(void) {
    int wanted = remote_players, n = remote_connected();
    switch (rg_host_state()) {
    case 1: return "Host a game: number of players";
    case 2: return "Hosting: choose your hero (Back = cancel)";
    case 3:
        if (!ws_up && no_relay) snprintf(status, sizeof status, "Cannot host: no server (no internet, and this device cannot run a LAN relay)");
        else if (!ws_up) snprintf(status, sizeof status, "Hosting: connecting to the server...");
        else snprintf(status, sizeof status, "Hosting, waiting for players %d/%d (others: Quick Match)", n, wanted);
        return status;
    case 4:
        if (n < wanted) snprintf(status, sizeof status, "Hosting: match running, %d/%d players still connected", n, wanted);
        else snprintf(status, sizeof status, "Hosting: match running");
        return status;
    }
    return hosting ? "F5 = host a new game" : status;
}

/* key filter in front of the engine (rg_key): F5 and the picker's keys */
int rg_server_key(int down, int k) {
    if (swallow_key && k == swallow_key) { if (!down) swallow_key = 0; return 1; }
    if (k == AWT_F5) { if (down && !test_mode) rg_host_request(); return 1; }
    if (!picker) return 0;
    if (!down) return 1;
    int n = picker;
    switch (k) {
    case 37: case 40: n--; break;                                   /* left, down */
    case 39: case 38: n++; break;                                   /* right, up */
    case '2': case '3': case '4': n = k - '0'; break;
    case 10: case 32: case 17:                                      /* enter, space, ctrl: continue */
        picker_default = picker;
        picker = 0;
        swallow_key = k;
        host_start(picker_default, hero_default, NULL);
        return 1;
    case 27: case 8:                                                /* escape, backspace: cancel */
        picker = 0;
        swallow_key = k;
        host_log("host: cancelled");
        return 1;
    default: return 1;
    }
    picker = n < 2 ? 2 : n > 4 ? 4 : n;
    return 1;
}

/* ------------------------------------------------------------------ drawing */
static void shade(uint32_t *px, int w, int x0, int y0, int x1, int y1, int keep) {   /* keep/256 of the colour */
    for (int y = y0; y < y1; y++)
        for (int x = x0; x < x1; x++) {
            uint32_t c = px[(size_t)y * w + x];
            px[(size_t)y * w + x] = (((c >> 16 & 0xff) * keep >> 8) << 16) | (((c >> 8 & 0xff) * keep >> 8) << 8) | ((c & 0xff) * keep >> 8);
        }
}
static void fill(uint32_t *px, int w, int x0, int y0, int x1, int y1, uint32_t c) {
    for (int y = y0; y < y1; y++)
        for (int x = x0; x < x1; x++) px[(size_t)y * w + x] = c;
}
static void centred(uint32_t *px, int w, int h, int y, int sc, const char *s, uint32_t c) {
    rt_draw_text(px, w, h, (w - rt_text_width(s, sc)) / 2, y, sc, s, c);
}
static void triangle(uint32_t *px, int w, int cx, int cy, int dir, int r, uint32_t c) {
    for (int dx = 0; dx <= r; dx++)                                 /* tip at cx + dir*r */
        for (int dy = -(r - dx); dy <= r - dx; dy++) px[(size_t)(cy + dy) * w + cx + dir * dx] = c;
}

const uint32_t *rg_server_compose(const uint32_t *fb, int w, int h) {
    int st = rg_host_state();
    if (st != 1 && st != 3) return fb;
    static uint32_t *buf;
    static size_t cap;
    size_t n = (size_t)w * h;
    if (cap < n) { free(buf); buf = (uint32_t *)malloc(n * 4); cap = buf ? n : 0; }
    if (!buf) return fb;
    memcpy(buf, fb, n * 4);
    const uint32_t gold = 0xffc828, white = 0xffffff, pale = 0xc8d2ff;
    if (st == 3) {                                                  /* status line at the top */
        const char *s = rg_host_status();
        shade(buf, w, 0, 0, w, 11 < h ? 11 : h, 80);
        centred(buf, w, h, 2, 1, s, white);
        return buf;
    }
    int u = h / 90 < 2 ? 2 : h / 90;                                /* 4 px at 360 rows */
    int bw = w - 4 * u < 110 * u ? w - 4 * u : 110 * u, bh = 58 * u;
    if (bh > h - 2 * u) bh = h - 2 * u;
    int x = (w - bw) / 2, y = (h - bh) / 2;
    shade(buf, w, 0, 0, w, h, 90);
    fill(buf, w, x, y, x + bw, y + bh, 0x182040);
    fill(buf, w, x, y, x + bw, y + 2, gold);
    fill(buf, w, x, y + bh - 2, x + bw, y + bh, gold);
    fill(buf, w, x, y, x + 2, y + bh, gold);
    fill(buf, w, x + bw - 2, y, x + bw, y + bh, gold);
    int sc = u >= 4 ? u / 2 : 1;                                    /* 2 at 360 rows */
    centred(buf, w, h, y + 4 * u, sc + 1, "HOST A GAME", gold);
    centred(buf, w, h, y + 12 * u, sc, "NUMBER OF PLAYERS", white);
    char d[2] = {(char)('0' + picker), 0};
    int big = 3 * sc + 1, cy = y + 26 * u;
    centred(buf, w, h, cy - 7 * big / 2, big, d, white);
    if (picker > 2) triangle(buf, w, w / 2 - 14 * u, cy, -1, 3 * u, gold);
    if (picker < 4) triangle(buf, w, w / 2 + 14 * u, cy, 1, 3 * u, gold);
    char line[80];
    snprintf(line, sizeof line, "YOU + %d OTHER PLAYER%s", picker - 1, picker == 2 ? "" : "S");
    centred(buf, w, h, y + 37 * u, sc, line, pale);
    centred(buf, w, h, y + 44 * u, 1, "THE OTHERS CHOOSE QUICK MATCH", pale);
    centred(buf, w, h, y + 50 * u, 1, "LEFT/RIGHT: CHANGE   OK/ENTER: CONTINUE   BACK/ESC: CANCEL", pale);
    return buf;
}

/* ------------------------------------------------------------------ glue */
void rg_server_init(void) {
    rt_hook_add(0x1008fa80, hook_menu);
    rt_hook_add(0x1008f500, hook_menu_ctor);
    /* engine fixes, every instance */
    rt_hook_add(0x100b2c66, hook_place_tile);
    rt_hook_add(0x10094eac, hook_member_clear);
    if (LD32(0x10188d70) == 0x100e16a0) ST32(0x10188d70, rt_stub_register("client.join", client_join, 0));
    else host_log("host: engine differs; going back into a match is not safe");
    /* hosting */
    rt_hook_add(0x100ca9d9, hook_host_hero);
    rt_hook_add(0x100caa30, hook_host_name);
    rt_hook_add(0x10023f40, hook_key_alloc);
    if (LD32(0x101779c4) == 0x1000e150) ST32(0x101779c4, rt_stub_register("server.command", ctrl_command, 0));
    else { rejoin_off = 1; host_log("host: engine differs; players who lose the connection are not held"); }
    uint32_t lvt = make_vtable("listener", 32);
    ST32(lvt + 0x00, rt_stub_register("listener.dtor", any_dtor, 0));
    ST32(lvt + 0x4c, rt_stub_register("listener.accept", l_accept, 0));
    ST32(lvt + 0x5c, rt_stub_register("listener.close", l_close, 0));
    listener_obj = rt_heap_alloc(0x20);
    memset(M + listener_obj, 0, 0x20);
    ST32(listener_obj, lvt);
    conn_vt = make_vtable("conn", 40);
    ST32(conn_vt + 0x00, rt_stub_register("conn.dtor", c_dtor, 0));
    addr_vt = make_vtable("addr", 24);
    ST32(addr_vt + 0x14, rt_stub_register("addr.nop", addr_nop, 0));
    ST32(conn_vt + 0x14, rt_stub_register("conn.address", c_address, 0));
    ST32(conn_vt + 0x18, rt_stub_register("conn.state", c_state, 0));
    ST32(conn_vt + 0x2c, rt_stub_register("conn.alive", c_alive, 0));
    ST32(conn_vt + 0x5c, rt_stub_register("conn.close", c_close, 0));
    ST32(conn_vt + 0x68, rt_stub_register("conn.send", c_send, 0));
    ST32(conn_vt + 0x7c, rt_stub_register("conn.recv", c_recv, 0));
    if (LD32(0x10188b68) == 0x100df500) ST32(0x10188b68, rt_stub_register("acceptor.setController", acc_set_controller, 0));
    else host_log("host: acceptor vtable differs; hosting will not work");
    hs_vt = rt_heap_alloc(13 * 4);
    memcpy(M + hs_vt, M + 0x101888e0, 13 * 4);
    ST32(hs_vt + 11 * 4, rt_stub_register("hostHeroSelect.onSelect", hs_on_select, 0));
    ST32(hs_vt + 12 * 4, rt_stub_register("hostHeroSelect.onBack", hs_on_back, 0));
    if (test_mode) host_log("host: test mode, relay %s room %s", net_server_url(), test_room);
}

void rg_server_pump(void) {
    uint32_t now = host_time_ms();
    static int frames, hosted;
    if (test_mode && !hosted && menu_base && ++frames > HOST_AFTER_FRAMES) {
        hosted = 1;
        host_start(test_players, -1, test_room);
    }
    /* The main menu replays the intro after ~35 s without input, and the picker keeps the keys
     * from the engine: tap Right (does nothing in the main menu) when it opens and every 10 s. */
    static uint32_t last_nudge;
    static int nudged_picker;
    if (picker && (!nudged_picker || now - last_nudge > 10000)) {
        nudged_picker = 1;
        last_nudge = now;
        rg_engine_key(1, 39);
        rg_engine_key(0, 39);
    }
    if (!picker) nudged_picker = 0;
#ifdef RT_NO_THREADS
    link_step();                         /* also after hosting ended: the closed link is let go */
#endif
    if (!hosting) return;
    static uint32_t retry, last_ping;
    if (!ws_up && !connecting && !ws && (now - retry > 2000 || !retry)) {
        retry = now ? now : 1;
        connecting = 1;
#ifdef RT_NO_THREADS
        link_start();
#else
        pthread_t t;
        if (pthread_create(&t, NULL, link_thread, NULL) == 0) pthread_detach(t);
        else connecting = 0, no_relay = 1;   /* threads cannot be started */
#endif
    }
    if (ws_up && now - last_ping > 15000) { last_ping = now; ws_send(1, "{\"t\":\"ping\"}", 12); }
    track_seats();
    if (serving && !match_started && remote_players > 0 && remote_connected() >= remote_players) match_started = 1;
    ev *e;
    while ((e = pop_ev())) {
        sess_t *s = by_pid(e->pid);
        if (e->kind == EV_JOIN) {
            if (s && s->closed) s = NULL;      /* the same relay pid in a new game */
            if (!s) {
                if (nsess < MAX_SESS) s = &sess[nsess++];
                else host_log("host: no player slot left");
                if (s) {
                    memset(s, 0, sizeof *s);
                    s->used = 1;
                    s->pid = e->pid;
                    if (e->len > 0) snprintf(s->name, sizeof s->name, "%.*s", e->len, (const char *)e->data);
                    host_log("host: player %d (%s) joined", e->pid, s->name);
                }
            }
            free(e);
        } else if (e->kind == EV_LEAVE || e->kind == EV_LOST) {
            if (s && e->kind == EV_LOST && serving && !rejoin_off && s->key > 0 && !s->closed) {
                s->away = 1;            /* the seat is held: see track_seats */
                s->away_since = host_time_ms();
                free_queue(s);
                host_log("host: player %d (%s) lost the connection; seat %d held", e->pid, s->name, s->slot);
            } else if (s) {
                s->closed = 1;
                host_log("host: player %d left", e->pid);
            }
            free(e);
        } else if (e->kind == EV_DATA && s) {
            e->next = NULL;
            if (s->in_tail) s->in_tail->next = e; else s->in_head = e;
            s->in_tail = e;
        } else if (e->kind == EV_CLOSED) {
            host_log("host: relay connection lost");
            pthread_mutex_lock(&send_lock);
            rt_link *l = ws;
            ws = NULL;
            pthread_mutex_unlock(&send_lock);
            if (l) { link_close(l); link_release(l); }
            for (int i = 0; i < nsess; i++) if (sess[i].used) sess[i].closed = 1;
            free(e);
        } else free(e);
    }
}
