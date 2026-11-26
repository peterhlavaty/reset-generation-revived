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
 * include the host (the relay's do not).
 *
 * The messages come on net.c's reader thread (the browser build: its game thread) and are only
 * taken while their relay link is the current one; the game thread works on a copy (sync_state).
 * The drawing measures every text for the screen size the engine was given: what does not fit
 * takes a shorter form, and a host's name is cut ("..") only when even that does not fit. */
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
static int shared_link;           /* the relay link whose messages count, 0 none (guarded by lock) */
static lobby_t cur;               /* the game thread's copy */
static int browsing;              /* seated, and the list is open again */
static uint32_t sel_id;           /* the selected game */
static int top;                   /* the first row shown */
static uint8_t held[256];         /* keys whose press was ours: their release is too */

/* ------------------------------------------------------------------ the relay's messages
 * A small reader for what serde_json writes, strict about the structure (a message it cannot
 * follow is logged and ignored, the list stays as it was) and bounded: no recursion deeper than
 * MAX_DEPTH, and at most MAX_GAMES entries are kept however long the message is. */
#define MAX_DEPTH 8

static const char *skip_ws(const char *p) {
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    return p;
}

/* p at a string's opening quote: just after its closing one, NULL if it has none */
static const char *skip_str(const char *p) {
    for (p++; *p; p++) {
        if (*p == '\\') {                                   /* the escaped character is skipped */
            if (!*++p) return NULL;
            continue;
        }
        if (*p == '"') return p + 1;
    }
    return NULL;
}

static const char *skip_value(const char *p, int depth) {
    p = skip_ws(p);
    if (*p == '"') return skip_str(p);
    if (*p == '[' || *p == '{') {
        char end = *p == '[' ? ']' : '}';
        if (depth >= MAX_DEPTH) return NULL;
        p = skip_ws(p + 1);
        if (*p == end) return p + 1;
        for (;;) {
            if (end == '}') {                               /* "key": */
                if (*p != '"' || !(p = skip_str(p))) return NULL;
                p = skip_ws(p);
                if (*p++ != ':') return NULL;
            }
            if (!(p = skip_value(p, depth + 1))) return NULL;
            p = skip_ws(p);
            if (*p == end) return p + 1;
            if (*p++ != ',') return NULL;
            p = skip_ws(p);
        }
    }
    const char *s = p;                                      /* a number, true, false, null */
    while ((*p >= '0' && *p <= '9') || (*p >= 'a' && *p <= 'z') || *p == '-' || *p == '+' || *p == '.' || *p == 'E') p++;
    return p == s ? NULL : p;
}

/* the value of a key of the message's object, NULL if it has none: keys are looked up by the
 * structure, so a name in a string can never pass for one */
static const char *field(const char *j, const char *key) {
    size_t kl = strlen(key);
    const char *p = skip_ws(j);
    if (*p++ != '{') return NULL;
    p = skip_ws(p);
    if (*p == '}') return NULL;
    for (;;) {
        if (*p != '"') return NULL;
        const char *k = p + 1, *e = skip_str(p);
        if (!e) return NULL;
        p = skip_ws(e);
        if (*p++ != ':') return NULL;
        p = skip_ws(p);
        if ((size_t)(e - 1 - k) == kl && !memcmp(k, key, kl)) return p;
        if (!(p = skip_value(p, 1))) return NULL;
        p = skip_ws(p);
        if (*p++ != ',') return NULL;
        p = skip_ws(p);
    }
}

static const char *num(const char *p, long long *v) {
    char *e;
    p = skip_ws(p);
    *v = strtoll(p, &e, 10);
    if (e == p) return NULL;
    while (*e == '.' || *e == 'e' || *e == 'E' || *e == '+' || *e == '-' || (*e >= '0' && *e <= '9')) e++;   /* 2.0, 2e0 */
    return skip_ws(e);
}

static int count(long long v) { return v < 0 ? 0 : v > 99 ? 99 : (int)v; }   /* a seat count to show */

/* [[ID,"host",hero,seated,seats,home,ask],...]; a later relay may add fields to an entry. The
 * player's own game (l->seat) is kept even past MAX_GAMES, so its line and row never go. */
static int parse_games(const char *p, lobby_t *l) {
    p = skip_ws(p);
    if (*p++ != '[') return 0;
    p = skip_ws(p);
    if (*p == ']') return 1;
    for (;;) {
        game_t g;
        long long v[6];
        memset(&g, 0, sizeof g);
        if (*p++ != '[' || !(p = num(p, &v[0])) || *p++ != ',') return 0;
        p = skip_ws(p);
        if (*p != '"') return 0;
        size_t i = 0;
        for (p++; *p && *p != '"'; p++) {
            if (*p == '\\' && p[1]) p++;                    /* an escaped character, taken as it is */
            if (i + 1 < sizeof g.host) g.host[i++] = *p;
        }
        if (*p++ != '"') return 0;
        for (int k = 1; k < 6; k++) {
            p = skip_ws(p);
            if (*p++ != ',' || !(p = num(p, &v[k]))) return 0;
        }
        while (*p == ',') {                                 /* a later relay's extra fields */
            if (!(p = skip_value(p + 1, 2))) return 0;
            p = skip_ws(p);
        }
        if (*p++ != ']') return 0;
        g.id = (uint32_t)v[0];
        g.in = count(v[2]);
        g.of = count(v[3]);
        g.home = v[4] != 0;
        g.ask = v[5] != 0;
        if (l->n < MAX_GAMES) l->g[l->n++] = g;
        else if (l->seat && g.id == l->seat) l->g[MAX_GAMES - 1] = g;
        p = skip_ws(p);
        if (*p != ',') return *p == ']';
        p = skip_ws(p + 1);
    }
}

void rg_lobby_message(int link, const char *j) {
    lobby_t l;
    memset(&l, 0, sizeof l);
    const char *on = field(j, "on");
    if (!on) {
        host_log("lobby: cannot read the game list: %.200s", j);
        return;
    }
    l.on = !strncmp(on, "true", 4);
    if (l.on) {
        const char *s = field(j, "seat"), *g = field(j, "games");
        long long seat = 0;
        if (s && !num(s, &seat)) seat = 0;
        l.seat = seat > 0 && seat <= 0xffffffffLL ? (uint32_t)seat : 0;
        if (!g || !parse_games(g, &l)) {
            host_log("lobby: cannot read the game list: %.200s", j);
            return;
        }
    }
    pthread_mutex_lock(&lock);
    if (link && link == shared_link) shared = l;
    pthread_mutex_unlock(&lock);
}

void rg_lobby_reset(int link) {
    pthread_mutex_lock(&lock);
    memset(&shared, 0, sizeof shared);
    shared_link = link;
    pthread_mutex_unlock(&lock);
}

void rg_lobby_lost(int link) {
    pthread_mutex_lock(&lock);
    if (link == shared_link) {
        memset(&shared, 0, sizeof shared);
        shared_link = 0;
    }
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
static const uint32_t gold = 0xffc828, white = 0xffffff, pale = 0xc8d2ff, dim = 0x8088a8;

static void upper(char *s) {
    for (; *s; s++)
        if (*s >= 'a' && *s <= 'z') *s = (char)(*s - 32);
}

/* the first of n texts that is at most max_w px wide at scale sc, else the last one cut to fit */
static const char *fit(char *buf, size_t size, const char *const *texts, int n, int sc, int max_w) {
    for (int i = 0; i < n - 1; i++)
        if (rt_text_width(texts[i], sc) <= max_w) return texts[i];
    snprintf(buf, size, "%s", texts[n - 1]);
    rt_text_fit(buf, sc, max_w);
    return buf;
}

/* The line at the top while the player sits in a game (g; NULL when the relay did not list it):
 * the whole sentence, a shorter one, the count with the keys, the count alone, then the count
 * with the host's name cut. Scale 1 like the host's status line (server.c). */
static void draw_strip(uint32_t *buf, int w, int h, const game_t *g) {
    char line[200], s[160], host[32];
    int max_w = w - 4;
    rt_shade_rect(buf, w, h, 0, 0, w, 11, 80);
    if (!g) {
        static const char *const t[] = {"YOU HAVE A SEAT IN A GAME.  UP/DOWN: OTHER GAMES", "UP/DOWN: OTHER GAMES"};
        rt_draw_text_centred(buf, w, h, 2, 1, fit(line, sizeof line, t, 2, 1, max_w), white);
        return;
    }
    snprintf(host, sizeof host, "%s", g->host);
    upper(host);
    int in = g->in + 1, of = g->of + 1, full = g->in >= g->of;
    for (int lv = 0; lv < 4; lv++) {
        if (lv >= 2) snprintf(s, sizeof s, "%s'S GAME %d/%d", host, in, of);
        else if (!g->ask) snprintf(s, sizeof s, "%s'S GAME %d/%d: STARTS WHEN %s.", host, in, of, lv ? "FULL" : "ALL SEATS ARE TAKEN");
        else if (!full && !lv) snprintf(s, sizeof s, "%s'S GAME %d/%d: %s STARTS IT WHEN ALL ARE IN.", host, in, of, host);
        else if (!full) snprintf(s, sizeof s, "%s'S GAME %d/%d: THE HOST STARTS IT.", host, in, of);
        else if (!lv) snprintf(s, sizeof s, "%s'S GAME IS FULL: WAITING FOR %s TO START IT.", host, host);
        else snprintf(s, sizeof s, "%s'S GAME IS FULL: WAITING FOR THE HOST.", host);
        snprintf(line, sizeof line, "%s%s", s, lv < 3 ? "  UP/DOWN: OTHER GAMES" : "");
        if (rt_text_width(line, 1) <= max_w) break;
        if (lv == 3) {
            char tail[40];
            snprintf(tail, sizeof tail, "'S GAME %d/%d", in, of);
            rt_text_fit(host, 1, max_w - (int)strlen(tail) * 6);   /* the name's room before the tail */
            snprintf(line, sizeof line, "%s%s", host, tail);
            rt_text_fit(line, 1, max_w);                             /* narrower than the count itself */
        }
    }
    rt_draw_text_centred(buf, w, h, 2, 1, line, white);
}

/* A row's columns at level lv: 0 "BOB'S GAME (HOME)" and "YOU ARE IN  2/4" / "2/4 PLAYERS",
 * 1 without "'S GAME", 2 the seats short too ("YOU 2/4" / "2/4"). */
static void row_text(const game_t *g, int own, int lv, char *left, size_t ln, char *right, size_t rn) {
    char host[32];
    snprintf(host, sizeof host, "%s", g->host);
    upper(host);
    snprintf(left, ln, lv ? "%s%s" : "%s'S GAME%s", host, g->home ? " (HOME)" : "");
    if (lv < 2) snprintf(right, rn, own ? "YOU ARE IN  %d/%d" : "%d/%d PLAYERS", g->in + 1, g->of + 1);
    else snprintf(right, rn, own ? "YOU %d/%d" : "%d/%d", g->in + 1, g->of + 1);
}

/* Level 3, where even level 2 does not fit: the seats stay whole (cut only on a screen narrower
 * than they are), the name is cut, and " (HOME)" goes when fewer than three letters of the name
 * would be left. room: the row's text width less the gap between the columns. */
static void fit_row(const game_t *g, char *left, size_t ln, char *right, int sc, int room) {
    room -= rt_text_fit(right, sc, room);
    if (rt_text_width(left, sc) <= room) return;
    char host[32];
    snprintf(host, sizeof host, "%s", g->host);
    upper(host);
    const char *home = g->home ? " (HOME)" : "";
    int name_w = room - (int)strlen(home) * 6 * sc;
    if (*home && name_w < rt_text_width("ABC..", sc)) {
        home = "";
        name_w = room;
    }
    if (rt_text_fit(host, sc, name_w) == 0) home = "";     /* no name left: no " (HOME)" without one */
    snprintf(left, ln, "%s%s", host, home);
}

const uint32_t *rg_lobby_compose(const uint32_t *fb, int w, int h) {
    sync_state();
    if (!cur.on || w <= 0 || h <= 0) return fb;
    static uint32_t *buf;
    static size_t cap;
    size_t n = (size_t)w * h;
    if (cap < n) { free(buf); buf = (uint32_t *)malloc(n * 4); cap = buf ? n : 0; }
    if (!buf) return fb;
    memcpy(buf, fb, n * 4);
    char line[200], left[48], right[40];
    int sel = selected();
    if (!list_open()) {                                     /* seated: a line at the top */
        int i = find(cur.seat);
        draw_strip(buf, w, h, i >= 0 ? &cur.g[i] : NULL);
        return buf;
    }
    /* sizes follow the height, and the width too, so a tall narrow screen keeps whole rows */
    int u = h / 90 < w / 160 ? h / 90 : w / 160;
    if (u < 2) u = 2;                                       /* 4 px at 640x360 */
    int sc = u >= 4 ? u / 2 : 1;                            /* 2 at 640x360 */
    int rowh = 6 * u, bw = w - 4 * u < 140 * u ? w - 4 * u : 140 * u, inner = bw - 4 * u;
    const char *title = cur.seat ? "OTHER GAMES" : "CHOOSE A GAME";
    int ts = sc + 1;
    while (ts > 1 && rt_text_width(title, ts) > inner) ts--;
    /* the title and "MORE ABOVE" above the rows; "MORE BELOW" and two lines below them (gap2
       apart); at 640x360 that is 56 px, 6 rows of 24 and 48 px, the panel 248 px high. A screen
       too low for that gets fewer rows, then (below about 120 rows) fewer lines. */
    int head = 4 * u + 7 * ts + 12 > 14 * u ? 4 * u + 7 * ts + 12 : 14 * u;
    int gap2 = 6 * u - 8 > 10 ? 6 * u - 8 : 10;
    int rows = ROWS, lines = 2, foot, bh;
    for (;;) {
        foot = lines == 2 ? (4 * u + 13 + gap2 > 12 * u ? 4 * u + 13 + gap2 : 12 * u) : lines ? 4 * u + 13 : 2 * u + 9;
        bh = head + rows * rowh + foot;
        if (bh <= h - 2 * u) break;
        if (rows > 1) rows--;
        else if (lines > 0) lines--;
        else break;
    }
    int x = (w - bw) / 2, y = (h - bh) / 2 > 0 ? (h - bh) / 2 : 0, ly = y + head;
    int keys_y = y + bh - 4 * u - 2, note_y = keys_y - gap2;
    rt_draw_panel(buf, w, h, x, y, bw, bh);
    snprintf(line, sizeof line, "%s", title);
    rt_text_fit(line, ts, inner);
    rt_draw_text_centred(buf, w, h, y + 4 * u, ts, line, gold);
    if (!cur.n) {
        /* also what a player reads while every waiting game is hidden from them (its host sent
           them away), so it does not say that no game waits */
        static const char *const none[] = {"NO GAME TO JOIN RIGHT NOW", "NO GAME TO JOIN"};
        static const char *const appear[] = {"GAMES WITH A FREE SEAT SHOW UP HERE BY THEMSELVES.", "NEW GAMES SHOW UP HERE."};
        static const char *const host_one[] = {"TO HOST ONE: F5 (ANDROID: HOST) IN THE MAIN MENU.", "HOST ONE IN THE MAIN MENU."};
        int e1 = rows >= 4 ? ly + rowh : ly, e2 = rows >= 4 ? ly + 3 * rowh : e1 + 7 * sc + 5;
        int end = ly + rows * rowh + 9;                     /* the rows and the unused "MORE BELOW" */
        rt_draw_text_centred(buf, w, h, e1, sc, fit(line, sizeof line, none, 2, sc, inner), white);
        if (e2 + 7 <= end) rt_draw_text_centred(buf, w, h, e2, 1, fit(line, sizeof line, appear, 2, 1, inner), pale);
        if (e2 + 19 <= end) rt_draw_text_centred(buf, w, h, e2 + 12, 1, fit(line, sizeof line, host_one, 2, 1, inner), pale);
    }
    /* the longest form of the columns that fits every row (one style for the whole list), else
       level 3: level 2 with the names cut where needed */
    int avail = bw - 12 * u, gap = 12 * sc;                 /* a row's text width; two glyphs between the columns */
    int lv = 0;
    for (; lv < 3; lv++) {
        int ok = 1;
        for (int i = 0; i < cur.n && ok; i++) {
            row_text(&cur.g[i], cur.g[i].id == cur.seat, lv, left, sizeof left, right, sizeof right);
            ok = rt_text_width(left, sc) + gap + rt_text_width(right, sc) <= avail;
        }
        if (ok) break;
    }
    if (sel < top) top = sel;
    if (sel >= top + rows) top = sel - rows + 1;
    if (top > cur.n - rows) top = cur.n - rows > 0 ? cur.n - rows : 0;
    for (int r = 0; r < rows && top + r < cur.n; r++) {
        const game_t *g = &cur.g[top + r];
        int own = g->id == cur.seat;
        int ry = ly + r * rowh, ty = ry + (rowh - u - 7 * sc) / 2;
        if (top + r == sel) rt_fill_rect(buf, w, h, x + 3 * u, ry, x + bw - 3 * u, ry + rowh - u, 0x34468a);
        row_text(g, own, lv < 3 ? lv : 2, left, sizeof left, right, sizeof right);
        if (lv == 3) fit_row(g, left, sizeof left, right, sc, avail - gap);
        rt_draw_text(buf, w, h, x + 6 * u, ty, sc, left, own ? gold : white);
        rt_draw_text(buf, w, h, x + bw - 6 * u - rt_text_width(right, sc), ty, sc, right, own ? gold : pale);
    }
    if (top > 0) rt_draw_text_centred(buf, w, h, ly - 9, 1, "MORE ABOVE", dim);
    if (top + rows < cur.n) rt_draw_text_centred(buf, w, h, ly + rows * rowh, 1, "MORE BELOW", dim);
    static const char *const starts[] = {"THE HOST STARTS THE GAME ONCE ALL SEATS ARE TAKEN", "THE HOST STARTS IT WHEN ALL ARE IN",
                                         "THE HOST STARTS IT"};
    static const char *const move_keys[] = {"UP/DOWN: CHOOSE   OK/ENTER: MOVE THERE   BACK/ESC: CLOSE",
                                            "UP/DOWN: CHOOSE  OK: MOVE  BACK: CLOSE", "OK: MOVE  BACK: CLOSE"};
    static const char *const join_keys[] = {"UP/DOWN: CHOOSE   OK/ENTER: JOIN   BACK/ESC: LEAVE QUICK MATCH",
                                            "UP/DOWN: CHOOSE  OK: JOIN  BACK: LEAVE", "OK: JOIN  BACK: LEAVE"};
    if (lines == 2) rt_draw_text_centred(buf, w, h, note_y, 1, fit(line, sizeof line, starts, 3, 1, inner), pale);
    if (lines) rt_draw_text_centred(buf, w, h, keys_y, 1, fit(line, sizeof line, cur.seat ? move_keys : join_keys, 3, 1, inner), pale);
    return buf;
}
