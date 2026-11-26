/* Relay client (WebSocket links: link.c, in the browser native/web/src/link.rs) and the SNAP
 * Mobile login/room state machine: on a worker thread, or without threads (the browser build,
 * RT_NO_THREADS) stepped on the game thread. */
#include "net.h"
#include "lobby.h"
#include "engine.h"
#include "host.h"
#include "link.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <signal.h>
#include <unistd.h>
#endif

#define REQUEST_TIMEOUT_MS 10000
#define MAX_PACKET 2048

static char server_url[256] = RG_ONLINE_SERVER;
static char nickname[32] = "";
static uint8_t *netbuf;
static int netbuf_size;

void rg_net_configure(const char *url, const char *nick) {
#ifndef _WIN32
    /* a send() on a connection the relay dropped must fail with EPIPE, not kill the game */
    signal(SIGPIPE, SIG_IGN);
#endif
    if (url) snprintf(server_url, sizeof server_url, "%s", url);
    if (nick) snprintf(nickname, sizeof nickname, "%s", nick);
}
void net_attach(uint8_t *buf, int size) { netbuf = buf; netbuf_size = size; }
const char *net_server_url(void) { return server_url; }
const char *net_nickname(void) {
    if (!nickname[0]) snprintf(nickname, sizeof nickname, "Player%04d", rand() % 9000 + 1000);
    return nickname;
}

/* ------------------------------------------------------------------ queues */
enum { A_LOGIN, A_LOGOUT, A_CREATE, A_JOIN, A_MOVE, A_LEAVE, A_SEND, A_FRIENDS };
enum { E_LOGIN_DONE, E_LOGIN_CANCELLED, E_CONNECTED, E_LEFT_ROOM, E_NAF, E_DATA, E_JOIN_RETRY, E_FRIENDS };
typedef struct item { struct item *next; int kind, arg, len; char name[72]; uint8_t data[]; } item;
typedef struct { item *head, *tail; } queue;

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t action_cv = PTHREAD_COND_INITIALIZER;
static pthread_cond_t reply_cv = PTHREAD_COND_INITIALIZER;
static queue actions, events;
static int running;

static item *mk(int kind, int arg, const char *name, const void *data, int len) {
    item *it = (item *)calloc(1, sizeof *it + (size_t)(len > 0 ? len : 0));
    it->kind = kind;
    it->arg = arg;
    it->len = len;
    if (name) snprintf(it->name, sizeof it->name, "%s", name);
    if (len > 0) memcpy(it->data, data, (size_t)len);
    return it;
}
static void push(queue *q, item *it) { if (q->tail) q->tail->next = it; else q->head = it; q->tail = it; }
static item *pop(queue *q) { item *it = q->head; if (it) { q->head = it->next; if (!q->head) q->tail = NULL; } return it; }

/* state (guarded by lock) */
static int logged_in, cancelled, logout_queued, login_in_progress, err_state, credentials_ok;
static int in_room, join_retries;
static char room[72];
static char entering[72];            /* the room enter_room is waiting for: see enter_room */

static void add_event(item *it) { pthread_mutex_lock(&lock); push(&events, it); pthread_mutex_unlock(&lock); }

static void add_action(item *it, int allow_logged_out) {
    pthread_mutex_lock(&lock);
    if (!allow_logged_out && (logout_queued || !logged_in)) { pthread_mutex_unlock(&lock); free(it); return; }
    if (it->kind == A_LOGOUT) logout_queued = 1;
    push(&actions, it);
    pthread_cond_signal(&action_cv);
    pthread_mutex_unlock(&lock);
}

/* ------------------------------------------------------------------ WebSocket */
static rt_link *ws;              /* the relay connection (guarded by send_lock) */
static char ws_url[256];         /* the relay it goes to (server=auto: found on login) */
static pthread_mutex_t send_lock = PTHREAD_MUTEX_INITIALIZER;
static char reply[512];          /* last control reply (JSON) */
static int reply_seq;
static int ws_generation;

static int ws_send(int opcode, const void *data, size_t len) {
    pthread_mutex_lock(&send_lock);
    rt_link *l = ws;
    if (l) link_ref(l);
    pthread_mutex_unlock(&send_lock);
    int ok = l && link_send(l, opcode, data, len);
    link_release(l);
    return ok;
}

/* tiny JSON field reader for the relay's flat messages */
static int json_str(const char *j, const char *key, char *out, size_t n) {
    char pat[40];
    if (!n) return 0;
    snprintf(pat, sizeof pat, "\"%s\":\"", key);
    const char *p = strstr(j, pat);
    if (!p) { out[0] = 0; return 0; }
    p += strlen(pat);
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < n) { if (*p == '\\' && p[1]) p++; out[i++] = *p++; }
    out[i] = 0;
    return 1;
}

static void on_disconnect(int gen) {
    rg_lobby_lost(gen);
    pthread_mutex_lock(&lock);
    int was = logged_in && gen == ws_generation && !logout_queued;
    pthread_mutex_unlock(&lock);
    if (was) {
        host_log("net: relay connection lost");
        add_event(mk(E_NAF, 0, NULL, NULL, 0));
        /* drop the pending actions but keep a queued logout (e.g. the engine's own) */
        pthread_mutex_lock(&lock);
        queue keep = {NULL, NULL};
        item *it;
        while ((it = pop(&actions))) { it->next = NULL; if (it->kind == A_LOGOUT) push(&keep, it); else free(it); }
        actions = keep;
        if (!actions.head) { logout_queued = 1; push(&actions, mk(A_LOGOUT, 0, NULL, NULL, 0)); pthread_cond_signal(&action_cv); }
        pthread_mutex_unlock(&lock);
    }
}

#ifdef RT_NO_THREADS
static char want[24];            /* the reply type the request in progress waits for (req_start) */
static char want_reply[512];     /* and the reply */
static int want_got;             /* 1 that reply came, -1 an error did */
#endif

/* a frame from the relay link `gen` (op 1 text, 2 binary; msg NUL-terminated) */
static void on_frame(int gen, int op, const uint8_t *msg, size_t len) {
    pthread_mutex_lock(&lock);
    int stale = gen != ws_generation;
    pthread_mutex_unlock(&lock);
    if (stale) return;                               /* the reader of a link replaced since */
    if (op == 1) {                                   /* text: control reply / notice */
        char t[24];
        json_str((const char *)msg, "t", t, sizeof t);
        if (!strcmp(t, "pong") || !strcmp(t, "peer_joined") || !strcmp(t, "peer_left")) return;
        if (!strcmp(t, "games")) { rg_lobby_message(gen, (const char *)msg); return; }   /* Quick Match: lobby.c */
        pthread_mutex_lock(&lock);
        if (!strcmp(t, "joined") && entering[0]) {   /* before the packets behind it */
            snprintf(room, sizeof room, "%s", entering);
            in_room = 1;
            entering[0] = 0;
            push(&events, mk(E_CONNECTED, 0, NULL, NULL, 0));
        }
        snprintf(reply, sizeof reply, "%s", (const char *)msg);
        reply_seq++;
#ifdef RT_NO_THREADS
        if (want[0] && (!strcmp(t, want) || !strcmp(t, "error"))) {
            want_got = !strcmp(t, want) ? 1 : -1;
            snprintf(want_reply, sizeof want_reply, "%s", (const char *)msg);
            want[0] = 0;
        }
#endif
        pthread_cond_broadcast(&reply_cv);
        pthread_mutex_unlock(&lock);
    } else {                                         /* binary: game packet */
        pthread_mutex_lock(&lock);
        int ok = in_room && len <= MAX_PACKET;
        pthread_mutex_unlock(&lock);
        if (ok) add_event(mk(E_DATA, 0, room, msg, (int)len));
    }
}

#ifndef RT_NO_THREADS
typedef struct { rt_link *l; int gen; } reader_arg;

static void *reader(void *arg) {
    reader_arg a = *(reader_arg *)arg;
    free(arg);
    uint8_t *msg = (uint8_t *)malloc(65536);
    size_t len;
    int op;
    while ((op = link_read(a.l, msg, 65536, &len)) != 0) on_frame(a.gen, op, msg, len);
    free(msg);
    link_release(a.l);
    on_disconnect(a.gen);
    return NULL;
}

/* sends a control message and waits for the reply of `type` (or an error); 1 on `type` */
static int request(const char *json, const char *type, char *out, size_t n) {
    pthread_mutex_lock(&lock);
    int seq = reply_seq;
    pthread_mutex_unlock(&lock);
    if (!ws_send(1, json, strlen(json))) return 0;
    struct timespec until;
    clock_gettime(CLOCK_REALTIME, &until);
    until.tv_sec += REQUEST_TIMEOUT_MS / 1000;
    pthread_mutex_lock(&lock);
    int got = 0;
    while (!got) {
        while (reply_seq == seq) {
            if (pthread_cond_timedwait(&reply_cv, &lock, &until) != 0) { pthread_mutex_unlock(&lock); return 0; }
        }
        seq = reply_seq;
        char t[24];
        json_str(reply, "t", t, sizeof t);
        if (!strcmp(t, type) || !strcmp(t, "error")) {
            got = !strcmp(t, type) ? 1 : -1;
            if (out) snprintf(out, n, "%s", reply);
        }
    }
    pthread_mutex_unlock(&lock);
    return got == 1;
}
#endif

/* Every logout and failed login ends here. The game list goes with the link: in the browser
   build nothing else would clear it (pump_link stops polling a closed link, so on_disconnect
   never runs), and a list left on screen kept the menu's keys. */
static void ws_close(void) {
    pthread_mutex_lock(&send_lock);
    rt_link *l = ws;
    ws = NULL;
    pthread_mutex_unlock(&send_lock);
    rg_lobby_reset(0);
    if (l) { link_close(l); link_release(l); }
}

#ifdef RT_NO_THREADS
static int link_dead;             /* the link ended (link_poll said so) */
#endif

static int ws_connect(void) {
    char found[128];
    const char *url = relay_for_joining(server_url, found, sizeof found);
    if (!url) { host_log("net: no game found on the LAN (server=%s)", server_url); return 0; }
    rt_link *l = link_open(url, "net");
    if (!l) return 0;
    snprintf(ws_url, sizeof ws_url, "%s", url);
    pthread_mutex_lock(&send_lock);
    ws = l;
    pthread_mutex_unlock(&send_lock);
    pthread_mutex_lock(&lock);
    int gen = ++ws_generation;
    pthread_mutex_unlock(&lock);
    rg_lobby_reset(gen);             /* before its reader runs: only this link's lists count now */
#ifdef RT_NO_THREADS
    link_dead = 0;
#else
    reader_arg *a = (reader_arg *)malloc(sizeof *a);
    a->l = l;
    a->gen = gen;
    link_ref(l);
    pthread_t t;
    pthread_create(&t, NULL, reader, a);
    pthread_detach(t);
#endif
    return 1;
}

/* ------------------------------------------------------------------ worker */
#ifndef RT_NO_THREADS
static int enter_room(const char *op, const char *name) {
    char json[160], out[512];
    if (in_room && strcmp(room, name) != 0) {
        request("{\"t\":\"leave\"}", "left", NULL, 0);
        add_event(mk(E_LEFT_ROOM, 0, room, NULL, 0));
        in_room = 0;
    }
    snprintf(json, sizeof json, "{\"t\":\"%s\",\"room\":\"%s\"}", op, name);
    /* The reader enters the room on the relay's "joined", not this thread after the reply: the
       casino and game rooms greet a joiner right behind it, and a greeting dropped as "not in a
       room" left the engine without the casino's hello, so it never started its keep-alive and
       dropped the connection 20 s later ("Connection..."). */
    pthread_mutex_lock(&lock);
    snprintf(entering, sizeof entering, "%s", name);
    pthread_mutex_unlock(&lock);
    int ok = request(json, "joined", out, sizeof out);
    pthread_mutex_lock(&lock);
    entering[0] = 0;
    pthread_mutex_unlock(&lock);
    if (ok) return 1;
    char code[32];
    json_str(out, "code", code, sizeof code);
    host_log("net: %s(%s) failed: %s", op, name, code[0] ? code : "timeout");
    in_room = 0;
    return 0;
}

static void do_login(void) {
    login_in_progress = 1;
    if (logged_in && ws) { credentials_ok = 1; add_event(mk(E_LOGIN_DONE, 0, NULL, NULL, 0)); login_in_progress = 0; return; }
    if (!nickname[0]) snprintf(nickname, sizeof nickname, "Player%04d", rand() % 9000 + 1000);
    if (!ws_connect()) { err_state = 3; login_in_progress = 0; return; }
    char json[120], out[512];
    snprintf(json, sizeof json, "{\"t\":\"hello\",\"v\":1,\"name\":\"%s\",\"pick\":true}", nickname);   /* pick: lobby.c */
    if (!request(json, "welcome", out, sizeof out)) {
        host_log("net: login rejected: %s", out);
        ws_close();
        err_state = 2;
        login_in_progress = 0;
        return;
    }
    char name[32];
    json_str(out, "name", name, sizeof name);
    host_log("net: logged in to %s as %s", ws_url, name);
    pthread_mutex_lock(&lock);
    logged_in = 1;
    credentials_ok = 1;
    pthread_mutex_unlock(&lock);
    add_event(mk(E_LOGIN_DONE, 0, NULL, NULL, 0));
    login_in_progress = 0;
}
#endif

static void do_logout(void) {
    pthread_mutex_lock(&lock);
    logged_in = 0;
    in_room = 0;
    credentials_ok = 0;
    pthread_mutex_unlock(&lock);
    ws_close();
    add_event(mk(E_LOGIN_CANCELLED, 0, NULL, NULL, 0));
    pthread_mutex_lock(&lock);
    logout_queued = 0;
    pthread_mutex_unlock(&lock);
}

#ifndef RT_NO_THREADS
static void *worker(void *arg) {
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&lock);
        while (!actions.head) pthread_cond_wait(&action_cv, &lock);
        item *it = pop(&actions);
        pthread_mutex_unlock(&lock);
        switch (it->kind) {
        case A_LOGIN: do_login(); break;
        case A_LOGOUT: do_logout(); break;
        case A_CREATE: enter_room("create", it->name); break;
        case A_MOVE: enter_room("move", it->name); break;
        case A_JOIN:
            if (!enter_room("join", it->name)) {
                if (join_retries < 4) {
#ifdef _WIN32
                    Sleep(3000);
#else
                    sleep(3);
#endif
                    join_retries++;
                    add_event(mk(E_JOIN_RETRY, 0, it->name, NULL, 0));
                } else {
                    err_state = 3;
                    add_event(mk(E_NAF, 2, NULL, NULL, 0));
                    add_action(mk(A_LOGOUT, 0, NULL, NULL, 0), 1);
                }
            }
            break;
        case A_LEAVE:
            if (in_room && !strcmp(room, it->name)) { request("{\"t\":\"leave\"}", "left", NULL, 0); in_room = 0; }
            add_event(mk(E_LEFT_ROOM, 0, it->name, NULL, 0));
            break;
        case A_SEND:
            if (!ws_send(2, it->data, (size_t)it->len)) on_disconnect(ws_generation);
            break;
        case A_FRIENDS: add_event(mk(E_FRIENDS, 0, NULL, NULL, 0)); break;
        }
        free(it);
    }
    return NULL;
}

static void *pinger(void *arg) {
    (void)arg;
    for (;;) {
#ifdef _WIN32
        Sleep(15000);
#else
        sleep(15);
#endif
        if (logged_in) ws_send(1, "{\"t\":\"ping\"}", 12);
    }
    return NULL;
}

#else
/* ------------------------------------------------------------------ worker without threads
 * The browser build: the same actions, one at a time, stepped on the game thread (net_step,
 * before every tick) instead of blocking: a request is sent (req_start) and its reply looked for
 * in the frames on_frame saw (req_poll). */
static item *cur;                /* the action in progress */
static int stage;
static uint32_t wait_until;

static int due(uint32_t t) { return (int32_t)(host_time_ms() - t) >= 0; }

static void req_start(const char *json, const char *type) {
    snprintf(want, sizeof want, "%s", type);
    want_got = 0;
    want_reply[0] = 0;
    wait_until = host_time_ms() + REQUEST_TIMEOUT_MS;
    if (!ws_send(1, json, strlen(json))) want_got = -1;
}
/* 0 still waiting, 1 the reply came (want_reply), -1 an error, a timeout or a lost link */
static int req_poll(void) {
    if (!want_got && (link_dead || !ws || due(wait_until))) want_got = -1;
    if (want_got) want[0] = 0;
    return want_got;
}

/* the frames that came in */
static void pump_link(void) {
    static uint8_t *msg;
    if (!msg) msg = (uint8_t *)malloc(65536);
    while (ws && !link_dead) {
        size_t len;
        int op = link_poll(ws, msg, 65536, &len);
        if (op == 0) return;
        if (op < 0) { link_dead = 1; on_disconnect(ws_generation); return; }
        on_frame(ws_generation, op, msg, len);
    }
}

static int step_login(void) {   /* 1 when done */
    char json[120];
    switch (stage) {
    case 0:
        login_in_progress = 1;
        if (logged_in && ws) { credentials_ok = 1; add_event(mk(E_LOGIN_DONE, 0, NULL, NULL, 0)); login_in_progress = 0; return 1; }
        net_nickname();
        if (!ws_connect()) { err_state = 3; login_in_progress = 0; return 1; }
        wait_until = host_time_ms() + REQUEST_TIMEOUT_MS;
        stage = 1;
        return 0;
    case 1: {
        int s = link_state(ws);
        if (s == 0 && !due(wait_until)) return 0;
        if (s != 1) { host_log("net: cannot connect to %s", ws_url); ws_close(); err_state = 3; login_in_progress = 0; return 1; }
        snprintf(json, sizeof json, "{\"t\":\"hello\",\"v\":1,\"name\":\"%s\",\"pick\":true}", nickname);   /* pick: lobby.c */
        req_start(json, "welcome");
        stage = 2;
        return 0;
    }
    default: {
        int r = req_poll();
        if (!r) return 0;
        if (r < 0) {
            host_log("net: login rejected: %s", want_reply);
            ws_close();
            err_state = 2;
            login_in_progress = 0;
            return 1;
        }
        char name[32];
        json_str(want_reply, "name", name, sizeof name);
        host_log("net: logged in to %s as %s", ws_url, name);
        logged_in = 1;
        credentials_ok = 1;
        add_event(mk(E_LOGIN_DONE, 0, NULL, NULL, 0));
        login_in_progress = 0;
        return 1;
    }
    }
}

/* enter_room in steps: 0 not yet, 1 in the room, -1 failed */
static int step_room(const char *op, const char *name) {
    char json[160];
    if (stage == 0) {
        if (in_room && strcmp(room, name) != 0) { req_start("{\"t\":\"leave\"}", "left"); stage = 1; return 0; }
        stage = 2;
    }
    if (stage == 1) {
        if (!req_poll()) return 0;
        add_event(mk(E_LEFT_ROOM, 0, room, NULL, 0));
        in_room = 0;
        stage = 2;
    }
    if (stage == 2) {
        snprintf(json, sizeof json, "{\"t\":\"%s\",\"room\":\"%s\"}", op, name);
        snprintf(entering, sizeof entering, "%s", name);   /* see enter_room */
        req_start(json, "joined");
        stage = 3;
        return 0;
    }
    int r = req_poll();
    if (!r) return 0;
    entering[0] = 0;
    if (r > 0) return 1;
    char code[32];
    json_str(want_reply, "code", code, sizeof code);
    host_log("net: %s(%s) failed: %s", op, name, code[0] ? code : "timeout");
    in_room = 0;
    return -1;
}

static int step(item *it) {     /* 1 when the action is done */
    switch (it->kind) {
    case A_LOGIN: return step_login();
    case A_LOGOUT: do_logout(); return 1;
    case A_CREATE: return step_room("create", it->name) != 0;
    case A_MOVE: return step_room("move", it->name) != 0;
    case A_JOIN:
        if (stage == 10) {                            /* waiting before the retry */
            if (!due(wait_until)) return 0;
            join_retries++;
            add_event(mk(E_JOIN_RETRY, 0, it->name, NULL, 0));
            return 1;
        }
        switch (step_room("join", it->name)) {
        case 0: return 0;
        case 1: return 1;
        }
        if (join_retries < 4) { stage = 10; wait_until = host_time_ms() + 3000; return 0; }
        err_state = 3;
        add_event(mk(E_NAF, 2, NULL, NULL, 0));
        add_action(mk(A_LOGOUT, 0, NULL, NULL, 0), 1);
        return 1;
    case A_LEAVE:
        if (stage == 0 && in_room && !strcmp(room, it->name)) { req_start("{\"t\":\"leave\"}", "left"); stage = 1; }
        if (stage == 1) {
            if (!req_poll()) return 0;
            in_room = 0;
        }
        add_event(mk(E_LEFT_ROOM, 0, it->name, NULL, 0));
        return 1;
    case A_SEND:
        if (!ws_send(2, it->data, (size_t)it->len)) on_disconnect(ws_generation);
        return 1;
    case A_FRIENDS: add_event(mk(E_FRIENDS, 0, NULL, NULL, 0)); return 1;
    }
    return 1;
}

static void net_step(void) {
    if (!running) return;
    pump_link();
    static uint32_t last_ping;
    if (logged_in && host_time_ms() - last_ping > 15000) { last_ping = host_time_ms(); ws_send(1, "{\"t\":\"ping\"}", 12); }
    for (int n = 0; n < 64; n++) {               /* the sends of a tick go out in that tick */
        if (!cur) {
            cur = pop(&actions);
            stage = 0;
            if (!cur) return;
        }
        if (!step(cur)) return;
        free(cur);
        cur = NULL;
    }
}
#endif

/* ------------------------------------------------------------------ engine-facing */
static int no_threads;   /* threads cannot be started: logins fail at once */

int net_init(void) {
    if (!running) {
        running = 1;
#ifndef RT_NO_THREADS
        pthread_t t;
        if (pthread_create(&t, NULL, worker, NULL) != 0) {
            no_threads = 1;
            host_log("net: online play is not available in this build");
            return 1;
        }
        pthread_detach(t);
        pthread_create(&t, NULL, pinger, NULL);
        pthread_detach(t);
#endif
    }
    return 1;
}

void net_start_login(void) { err_state = 0; credentials_ok = 0; cancelled = 0; }
void net_stop_login(void) {}
int net_login_status(void) {
    if (err_state == 3) return 7;
    if (err_state == 1) return 6;
    if (err_state == 2) return 5;
    if (cancelled && login_in_progress) return 1;
    if (cancelled) return 2;
    if (credentials_ok) return 4;
    return 3;
}
int net_login_cancelled(void) { return cancelled; }
int net_is_logged_in(void) { return logged_in; }
/* A new attempt: the engine logs in with login() and never calls startLoginScreen(), so an
 * earlier failure ("cannot connect") or logout ("cancelled") must not answer for this one. */
int net_login(void) {
    err_state = 0;
    cancelled = 0;
    if (no_threads) { err_state = 3; return 1; }
    login_in_progress = 1;
    add_action(mk(A_LOGIN, 0, NULL, NULL, 0), 1);
    return 1;
}
void net_logout(void) { add_action(mk(A_LOGOUT, 0, NULL, NULL, 0), 1); cancelled = 1; }
void net_room(const char *op, const char *name) {
    if (!name) return;
    if (!strcmp(op, "join")) join_retries = 0;
    add_action(mk(!strcmp(op, "create") ? A_CREATE : !strcmp(op, "join") ? A_JOIN : A_MOVE, 0, name, NULL, 0), 0);
}
void net_leave(const char *name) { add_action(mk(A_LEAVE, 0, name ? name : "", NULL, 0), 0); }
int net_send(int len) {
    if (len > 0 && len <= netbuf_size) add_action(mk(A_SEND, 0, NULL, netbuf, len), 0);
    return len;
}
int net_control(const char *json) { return ws_send(1, json, strlen(json)); }
void net_friend_list(void) { add_action(mk(A_FRIENDS, 0, NULL, NULL, 0), 0); }

void net_dispatch_events(void) {
#ifdef RT_NO_THREADS
    net_step();
#endif
    for (;;) {
        pthread_mutex_lock(&lock);
        item *it = pop(&events);
        pthread_mutex_unlock(&lock);
        if (!it) return;
        switch (it->kind) {
        case E_LOGIN_DONE: rg_ev_simple("onArenaLoginCompleted"); break;
        case E_LOGIN_CANCELLED: rg_ev_simple("onArenaLoginCancelled"); break;
        case E_CONNECTED: rg_ev_simple("onConnected"); break;
        case E_LEFT_ROOM: rg_ev_str("onDisconnectedFromGameRoom", it->name); break;
        case E_NAF: rg_ev_int("onDisconnectedFromNAF", it->arg); break;
        case E_DATA:
            if (netbuf && it->len <= netbuf_size) {
                memcpy(netbuf, it->data, (size_t)it->len);
                if (getenv("RT_NET_DEBUG")) host_log("net: data %d bytes for room \"%s\"", it->len, it->name);
                rg_ev_data(it->len, it->name);
            }
            break;
        case E_JOIN_RETRY: add_action(mk(A_JOIN, 0, it->name, NULL, 0), 0); break;
        case E_FRIENDS: rg_ev_friend_list_empty(); break;
        }
        free(it);
    }
}
