/* Choosing a hosted game in Quick Match (games since 2026-11-25; the relay's side is
 * server/src/casino.rs, docs/MULTIPLAYER.md "Choosing a game").
 *
 * While the player waits on the engine's "Players in Match" screen, the relay sends the hosted
 * games that wait for players and the one the player sits in:
 *   {"t":"games","on":true,"seat":ID,"games":[[ID,"host",hero,seated,seats,home,ask],...]}
 * (home: hosted from the player's own network; ask: its host starts it) and {"on":false} once the
 * player no longer waits. Without a seat a list is drawn over the screen: Up/Down and OK pick a
 * game ({"t":"pick","id":ID}); Back leaves Quick Match as before. With a seat (picked, or a game
 * from home) a line at the top says whose game it is, and Up/Down open the list again (Back closes
 * it). The engine's own list then shows the host and the players seated with them. Counts shown
 * include the host (the relay's do not). */
#include "lobby.h"
#include "host.h"
#include "net.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_GAMES 20
#define ROWS 6

typedef struct { uint32_t id; char host[24]; int in, of, home, ask; } game_t;
typedef struct { int on, n; uint32_t seat; game_t g[MAX_GAMES]; } lobby_t;

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static lobby_t shared;            /* the relay's last word (guarded by lock) */
static lobby_t cur;               /* the game thread's copy */
static int browsing;              /* seated, and the list is open again */
static uint32_t sel_id;           /* the selected game */
static int top;                   /* the first row shown */
static uint8_t held[256];         /* keys whose press was ours: their release is too */

/* ------------------------------------------------------------------ the relay's messages */
static const char *num(const char *p, long *v) {
    char *e;
    *v = strtol(p, &e, 10);
    return e == p ? NULL : e;
}

/* [[ID,"host",hero,seated,seats,home,ask],...] as serde_json writes it (no spaces) */
static int parse_games(const char *p, lobby_t *l) {
    if (*p++ != '[') return 0;
    if (*p == ']') return 1;
    for (;;) {
        game_t g;
        long v[6];
        memset(&g, 0, sizeof g);
        if (*p++ != '[' || !(p = num(p, &v[0])) || *p++ != ',' || *p++ != '"') return 0;
        size_t i = 0;
        for (; *p && *p != '"'; p++)
            if (i + 1 < sizeof g.host) g.host[i++] = *p;
        if (*p++ != '"') return 0;
        for (int k = 1; k < 6; k++)
            if (*p++ != ',' || !(p = num(p, &v[k]))) return 0;
        if (*p == ',') {                                     /* a later relay's extra fields */
            while (*p && *p != ']') p++;
        }
        if (*p++ != ']') return 0;
        g.id = (uint32_t)v[0];
        g.in = (int)v[2];
        g.of = (int)v[3];
        g.home = (int)v[4];
        g.ask = (int)v[5];
        if (l->n < MAX_GAMES) l->g[l->n++] = g;
        if (*p != ',') return *p == ']';
        p++;
    }
}

void rg_lobby_message(const char *j) {
    lobby_t l;
    memset(&l, 0, sizeof l);
    l.on = strstr(j, "\"on\":true") != NULL;
    if (l.on) {
        const char *s = strstr(j, "\"seat\":"), *g = strstr(j, "\"games\":");
        long seat = 0;
        if (s) num(s + 7, &seat);
        l.seat = (uint32_t)seat;
        if (!g || !parse_games(g + 8, &l)) {
            host_log("lobby: cannot read the game list: %.200s", j);
            return;
        }
    }
    pthread_mutex_lock(&lock);
    shared = l;
    pthread_mutex_unlock(&lock);
}

void rg_lobby_reset(void) {
    pthread_mutex_lock(&lock);
    memset(&shared, 0, sizeof shared);
    pthread_mutex_unlock(&lock);
}

static void sync_state(void) {
    pthread_mutex_lock(&lock);
    cur = shared;
    pthread_mutex_unlock(&lock);
    if (!cur.on || !cur.seat) browsing = 0;
}

static int list_open(void) { return cur.on && (!cur.seat || browsing); }

static int find(uint32_t id) {
    for (int i = 0; i < cur.n; i++)
        if (cur.g[i].id == id) return i;
    return -1;
}

/* the selected row (the player's game, else the first, if the selected one is gone) */
static int selected(void) {
    int i = find(sel_id);
    if (i < 0) i = find(cur.seat);
    if (i < 0) i = 0;
    if (cur.n) sel_id = cur.g[i].id;
    return i;
}

/* ------------------------------------------------------------------ keys */
int rg_lobby_key(int down, int k) {
    if (k < 0 || k > 255) return 0;
    if (!down) {
        if (!held[k]) return 0;
        held[k] = 0;
        return 1;
    }
    sync_state();
    if (!cur.on) return 0;
    int i = selected();
    switch (k) {
    case 38: case 40:                                       /* up, down */
        if (!list_open()) {
            browsing = 1;
            sel_id = cur.seat;
        } else if (cur.n) {
            i += k == 40 ? 1 : -1;
            sel_id = cur.g[i < 0 ? 0 : i >= cur.n ? cur.n - 1 : i].id;
        }
        break;
    case 10: case 32: case 17:                              /* enter, space, ctrl (Android: OK) */
        if (!list_open()) return 0;
        if (cur.n && cur.g[i].id != cur.seat) {
            char json[64];
            snprintf(json, sizeof json, "{\"t\":\"pick\",\"id\":%u}", cur.g[i].id);
            net_control(json);
            host_log("lobby: joining %s's game", cur.g[i].host);
        }
        browsing = 0;
        break;
    case 27: case 8:                                        /* back: closes the list ... */
        if (!(cur.seat && browsing)) return 0;              /* ... or leaves Quick Match (the engine) */
        browsing = 0;
        break;
    default:
        return 0;
    }
    held[k] = 1;
    return 1;
}

/* ------------------------------------------------------------------ drawing */
static void upper(char *s) {
    for (; *s; s++)
        if (*s >= 'a' && *s <= 'z') *s = (char)(*s - 32);
}

const uint32_t *rg_lobby_compose(const uint32_t *fb, int w, int h) {
    sync_state();
    if (!cur.on) return fb;
    static uint32_t *buf;
    static size_t cap;
    size_t n = (size_t)w * h;
    if (cap < n) { free(buf); buf = (uint32_t *)malloc(n * 4); cap = buf ? n : 0; }
    if (!buf) return fb;
    memcpy(buf, fb, n * 4);
    const uint32_t gold = 0xffc828, white = 0xffffff, pale = 0xc8d2ff, dim = 0x8088a8;
    char line[160], host[32];
    int sel = selected();
    if (!list_open()) {                                     /* seated: a line at the top */
        int i = find(cur.seat);
        if (i < 0) return fb;
        const game_t *g = &cur.g[i];
        snprintf(host, sizeof host, "%s", g->host);
        upper(host);
        if (!g->ask) snprintf(line, sizeof line, "%s'S GAME %d/%d: STARTS WHEN ALL SEATS ARE TAKEN.  UP/DOWN: OTHER GAMES", host, g->in + 1, g->of + 1);
        else if (g->in < g->of) snprintf(line, sizeof line, "%s'S GAME %d/%d: %s STARTS IT WHEN ALL ARE IN.  UP/DOWN: OTHER GAMES", host, g->in + 1, g->of + 1, host);
        else snprintf(line, sizeof line, "%s'S GAME IS FULL: WAITING FOR %s TO START IT.  UP/DOWN: OTHER GAMES", host, host);
        rt_shade_rect(buf, w, h, 0, 0, w, 11, 80);
        rt_draw_text_centred(buf, w, h, 2, 1, line, white);
        return buf;
    }
    int u = h / 90 < 2 ? 2 : h / 90;                        /* 4 px at 360 rows */
    int sc = u >= 4 ? u / 2 : 1;                            /* 2 at 360 rows */
    int rowh = 6 * u, bw = w - 4 * u < 140 * u ? w - 4 * u : 140 * u, bh = 26 * u + ROWS * rowh;
    if (bh > h - 2 * u) bh = h - 2 * u;
    int x = (w - bw) / 2, y = (h - bh) / 2, ly = y + 14 * u;
    rt_draw_panel(buf, w, h, x, y, bw, bh);
    rt_draw_text_centred(buf, w, h, y + 4 * u, sc + 1, cur.seat ? "OTHER GAMES" : "CHOOSE A GAME", gold);
    if (!cur.n) {
        rt_draw_text_centred(buf, w, h, ly + rowh, sc, "NO GAME IS WAITING FOR PLAYERS", white);
        rt_draw_text_centred(buf, w, h, ly + 3 * rowh, 1, "A FRIEND HOSTS ONE: F5 (ANDROID: HOST) IN THE MAIN MENU.", pale);
        rt_draw_text_centred(buf, w, h, ly + 3 * rowh + 12, 1, "IT SHOWS UP HERE BY ITSELF.", pale);
    }
    if (sel < top) top = sel;
    if (sel >= top + ROWS) top = sel - ROWS + 1;
    if (top > cur.n - ROWS) top = cur.n - ROWS > 0 ? cur.n - ROWS : 0;
    for (int r = 0; r < ROWS && top + r < cur.n; r++) {
        const game_t *g = &cur.g[top + r];
        int ry = ly + r * rowh, ty = ry + (rowh - u - 7 * sc) / 2;
        if (top + r == sel) rt_fill_rect(buf, w, h, x + 3 * u, ry, x + bw - 3 * u, ry + rowh - u, 0x34468a);
        snprintf(host, sizeof host, "%s", g->host);
        upper(host);
        snprintf(line, sizeof line, "%s'S GAME%s", host, g->home ? " (HOME)" : "");
        rt_draw_text(buf, w, h, x + 6 * u, ty, sc, line, g->id == cur.seat ? gold : white);
        if (g->id == cur.seat) snprintf(line, sizeof line, "YOU ARE IN  %d/%d", g->in + 1, g->of + 1);
        else snprintf(line, sizeof line, "%d/%d PLAYERS", g->in + 1, g->of + 1);
        rt_draw_text(buf, w, h, x + bw - 6 * u - rt_text_width(line, sc), ty, sc, line, g->id == cur.seat ? gold : pale);
    }
    if (top > 0) rt_draw_text_centred(buf, w, h, ly - 9, 1, "MORE ABOVE", dim);
    if (top + ROWS < cur.n) rt_draw_text_centred(buf, w, h, ly + ROWS * rowh, 1, "MORE BELOW", dim);
    rt_draw_text_centred(buf, w, h, y + bh - 10 * u + 6, 1, "THE HOST STARTS THE GAME ONCE ALL SEATS ARE TAKEN", pale);
    rt_draw_text_centred(buf, w, h, y + bh - 4 * u - 2, 1,
                         cur.seat ? "UP/DOWN: CHOOSE   OK/ENTER: MOVE THERE   BACK/ESC: CLOSE"
                                  : "UP/DOWN: CHOOSE   OK/ENTER: JOIN   BACK/ESC: LEAVE QUICK MATCH", pale);
    return buf;
}
