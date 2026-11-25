/* Relay connections: WebSocket client links (ws://, and wss:// through BearSSL) and the relay
 * choice (link.h). */
#include "link.h"
#include "engine.h"
#include "host.h"
#include "bearssl.h"
#include "tls_roots.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
typedef SOCKET sock_t;
#define BAD_SOCK INVALID_SOCKET
#define closesock closesocket
#define SHUT_RDWR SD_BOTH
#else
#include <errno.h>
#include <fcntl.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
typedef int sock_t;
#define BAD_SOCK (-1)
#define closesock close
#endif

#define CONNECT_TIMEOUT_MS 8000

static void net_startup(void) {
#ifdef _WIN32
    static int done;
    if (!done) { WSADATA w; WSAStartup(MAKEWORD(2, 2), &w); done = 1; }
#endif
}

static void set_blocking(sock_t s, int on) {
#ifdef _WIN32
    u_long nb = !on;
    ioctlsocket(s, FIONBIO, &nb);
#else
    int f = fcntl(s, F_GETFL, 0);
    fcntl(s, F_SETFL, on ? f & ~O_NONBLOCK : f | O_NONBLOCK);
#endif
}

static void set_timeout(sock_t s, int opt, int ms) {
#ifdef _WIN32
    DWORD t = (DWORD)ms;
#else
    struct timeval t = {ms / 1000, (ms % 1000) * 1000};
#endif
    setsockopt(s, SOL_SOCKET, opt, (const char *)&t, sizeof t);
}

/* ------------------------------------------------------------------ addresses */
typedef struct { char host[200], path[200]; int port, tls, own_port; } url_parts;

static int parse_url(const char *url, url_parts *u) {
    memset(u, 0, sizeof *u);
    const char *p;
    if (!strncmp(url, "ws://", 5)) { p = url + 5; u->port = 80; }
    else if (!strncmp(url, "wss://", 6)) { p = url + 6; u->port = 443; u->tls = 1; }
    else return 0;
    const char *slash = strchr(p, '/'), *colon = strchr(p, ':');
    size_t hl = slash ? (size_t)(slash - p) : strlen(p);
    if (colon && (!slash || colon < slash)) { hl = (size_t)(colon - p); u->port = atoi(colon + 1); u->own_port = 1; }
    snprintf(u->host, sizeof u->host, "%.*s", (int)hl, p);
    snprintf(u->path, sizeof u->path, "%s", slash ? slash : "/");
    return u->host[0] && u->port > 0;
}

/* a blocking TCP connection, or BAD_SOCK after timeout_ms */
static sock_t tcp_connect(const char *host, int port, int timeout_ms) {
    net_startup();
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    char ports[8];
    snprintf(ports, sizeof ports, "%d", port);
    if (getaddrinfo(host, ports, &hints, &res) != 0 || !res) return BAD_SOCK;
    sock_t s = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (s == BAD_SOCK) { freeaddrinfo(res); return BAD_SOCK; }
    set_blocking(s, 0);
    int r = connect(s, res->ai_addr, (int)res->ai_addrlen);
    freeaddrinfo(res);
    if (r != 0) {
        int err = 0, ready;
        socklen_t el = sizeof err;
#ifdef _WIN32
        fd_set w, e;
        FD_ZERO(&w); FD_SET(s, &w);
        FD_ZERO(&e); FD_SET(s, &e);
        struct timeval tv = {timeout_ms / 1000, (timeout_ms % 1000) * 1000};
        ready = select(0, NULL, &w, &e, &tv) > 0 && !FD_ISSET(s, &e);
#else
        /* poll, not select: the game has many files open (Android: fds past FD_SETSIZE abort) */
        struct pollfd p = {s, POLLOUT, 0};
        ready = poll(&p, 1, timeout_ms) > 0 && !(p.revents & (POLLERR | POLLHUP));
#endif
        if (!ready || getsockopt(s, SOL_SOCKET, SO_ERROR, (char *)&err, &el) != 0 || err) {
            closesock(s);
            return BAD_SOCK;
        }
    }
    set_blocking(s, 1);
    int one = 1;
    setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
    return s;
}

static int send_all(sock_t s, const uint8_t *p, size_t n) {
    while (n) {
        int k = (int)send(s, (const char *)p, (int)n, 0);
        if (k <= 0) return 0;
        p += k;
        n -= (size_t)k;
    }
    return 1;
}

/* 1 when s has data (or its end) within ms, 0 on timeout, -1 on error */
static int wait_readable(sock_t s, int ms) {
#ifdef _WIN32
    fd_set r;
    FD_ZERO(&r);
    FD_SET(s, &r);
    struct timeval tv = {ms / 1000, (ms % 1000) * 1000};
    return select(0, &r, NULL, NULL, &tv);
#else
    struct pollfd p = {s, POLLIN, 0};
    int k = poll(&p, 1, ms);
    return k < 0 ? (errno == EINTR ? 0 : -1) : k;
#endif
}

static void nap(void) {
#ifdef _WIN32
    Sleep(1);
#else
    usleep(1000);
#endif
}

/* ------------------------------------------------------------------ links */
/* wss:// is TLS 1.2 by BearSSL (downloaded by rgtool), trusting Let's Encrypt's roots
 * (tls_roots.h). A link's engine is shared by its reader thread and its senders under tls_lock;
 * nobody holds the lock while waiting for the network. */
typedef struct {
    br_ssl_client_context cc;
    br_x509_minimal_context xc;
    unsigned char buf[BR_SSL_BUFSIZE_BIDI];
} tls_conn;

struct rt_link {
    sock_t s;
    int refs, closed;
    uint32_t deadline;          /* reads fail after this host_time_ms() (while connecting); 0: never */
    pthread_mutex_t send_lock;  /* one WebSocket frame at a time */
    pthread_mutex_t tls_lock;
    tls_conn *tls;              /* NULL for ws:// */
};

static br_x509_trust_anchor anchors[sizeof TLS_ROOTS / sizeof TLS_ROOTS[0]];
static size_t n_anchors;
static pthread_once_t anchors_once = PTHREAD_ONCE_INIT;

typedef struct { unsigned char *p; size_t n; } bytes;

static void dn_add(void *ctx, const void *buf, size_t len) {
    bytes *b = (bytes *)ctx;
    b->p = (unsigned char *)realloc(b->p, b->n + len);
    memcpy(b->p + b->n, buf, len);
    b->n += len;
}

static unsigned char *copy_of(const unsigned char *p, size_t n) {
    unsigned char *q = (unsigned char *)malloc(n ? n : 1);
    memcpy(q, p, n);
    return q;
}

static void load_anchors(void) {
    for (size_t i = 0; i < sizeof TLS_ROOTS / sizeof TLS_ROOTS[0]; i++) {
        br_x509_decoder_context dc;
        bytes dn = {NULL, 0};
        br_x509_decoder_init(&dc, dn_add, &dn);
        br_x509_decoder_push(&dc, TLS_ROOTS[i].der, TLS_ROOTS[i].len);
        br_x509_pkey *pk = br_x509_decoder_get_pkey(&dc);
        if (!pk || br_x509_decoder_last_error(&dc) || !br_x509_decoder_isCA(&dc)) { free(dn.p); continue; }
        br_x509_trust_anchor *ta = &anchors[n_anchors++];
        ta->dn.data = dn.p;
        ta->dn.len = dn.n;
        ta->flags = BR_X509_TA_CA;
        ta->pkey = *pk;   /* the key bytes live in the decoder: copy them */
        if (pk->key_type == BR_KEYTYPE_RSA) {
            ta->pkey.key.rsa.n = copy_of(pk->key.rsa.n, pk->key.rsa.nlen);
            ta->pkey.key.rsa.e = copy_of(pk->key.rsa.e, pk->key.rsa.elen);
        } else {
            ta->pkey.key.ec.q = copy_of(pk->key.ec.q, pk->key.ec.qlen);
        }
    }
}

static int expired(rt_link *l) { return l->deadline && (int32_t)(host_time_ms() - l->deadline) >= 0; }

/* The TLS steps below run with tls_lock held. */
static int tls_flush(rt_link *l) {
    br_ssl_engine_context *e = &l->tls->cc.eng;
    while (br_ssl_engine_current_state(e) & BR_SSL_SENDREC) {
        size_t n;
        unsigned char *b = br_ssl_engine_sendrec_buf(e, &n);
        if (!send_all(l->s, b, n)) return 0;
        br_ssl_engine_sendrec_ack(e, n);
    }
    return 1;
}

/* Feeds the engine from the socket, waiting up to 100 ms for data (without the lock: the reader
 * thread also gets here, and another thread may have fed the engine meanwhile). */
static int tls_fill(rt_link *l) {
    br_ssl_engine_context *e = &l->tls->cc.eng;
    pthread_mutex_unlock(&l->tls_lock);
    int r = wait_readable(l->s, 100);
    pthread_mutex_lock(&l->tls_lock);
    if (r < 0 || (r == 0 && expired(l))) return 0;
    if (!(br_ssl_engine_current_state(e) & BR_SSL_RECVREC) || wait_readable(l->s, 0) <= 0) return 1;
    size_t n;
    unsigned char *b = br_ssl_engine_recvrec_buf(e, &n);
    int k = (int)recv(l->s, (char *)b, (int)n, 0);
    if (k <= 0) return 0;
    br_ssl_engine_recvrec_ack(e, (size_t)k);
    return 1;
}

/* One step on: send what is pending, or read; 0 once the connection is over. */
static int tls_step(rt_link *l) {
    unsigned st = br_ssl_engine_current_state(&l->tls->cc.eng);
    if (st & BR_SSL_CLOSED) return 0;
    if (st & BR_SSL_SENDREC) return tls_flush(l);
    if (st & BR_SSL_RECVREC) return tls_fill(l);
    pthread_mutex_unlock(&l->tls_lock);   /* waiting for another thread to take received data */
    nap();
    pthread_mutex_lock(&l->tls_lock);
    return 1;
}

/* The handshake: 0, or BearSSL's error (-1: the connection ended). */
static int tls_start(rt_link *l, const char *host) {
    pthread_once(&anchors_once, load_anchors);
    tls_conn *t = (tls_conn *)calloc(1, sizeof *t);
    l->tls = t;
    br_ssl_engine_context *e = &t->cc.eng;
    br_ssl_client_init_full(&t->cc, &t->xc, anchors, n_anchors);
    br_ssl_engine_set_buffer(e, t->buf, sizeof t->buf, 1);
    if (!br_ssl_client_reset(&t->cc, host, 0)) return br_ssl_engine_last_error(e);
    pthread_mutex_lock(&l->tls_lock);
    int ok = 1;
    while (ok && !(br_ssl_engine_current_state(e) & (BR_SSL_SENDAPP | BR_SSL_RECVAPP))) ok = tls_step(l);
    pthread_mutex_unlock(&l->tls_lock);
    int err = br_ssl_engine_last_error(e);
    return ok ? 0 : err ? err : -1;
}

static int tls_send(rt_link *l, const uint8_t *p, size_t n) {
    br_ssl_engine_context *e = &l->tls->cc.eng;
    int ok = 1;
    pthread_mutex_lock(&l->tls_lock);
    while (ok && n) {
        if (br_ssl_engine_current_state(e) & BR_SSL_SENDAPP) {
            size_t k;
            unsigned char *b = br_ssl_engine_sendapp_buf(e, &k);
            if (k > n) k = n;
            memcpy(b, p, k);
            br_ssl_engine_sendapp_ack(e, k);
            p += k;
            n -= k;
        } else {
            ok = tls_step(l);
        }
    }
    if (ok) {
        br_ssl_engine_flush(e, 0);
        ok = tls_flush(l);
    }
    pthread_mutex_unlock(&l->tls_lock);
    return ok;
}

static int tls_recv_some(rt_link *l, uint8_t *p, size_t n) {
    br_ssl_engine_context *e = &l->tls->cc.eng;
    int got = 0;
    pthread_mutex_lock(&l->tls_lock);
    for (;;) {
        if (br_ssl_engine_current_state(e) & BR_SSL_RECVAPP) {
            size_t k;
            unsigned char *b = br_ssl_engine_recvapp_buf(e, &k);
            if (k > n) k = n;
            memcpy(p, b, k);
            br_ssl_engine_recvapp_ack(e, k);
            got = (int)k;
            break;
        }
        if (!tls_step(l)) break;
    }
    pthread_mutex_unlock(&l->tls_lock);
    return got;
}

static int raw_send(rt_link *l, const uint8_t *p, size_t n) {
    return l->tls ? tls_send(l, p, n) : send_all(l->s, p, n);
}

/* Some bytes (> 0), or 0 once the connection is over. */
static int raw_recv_some(rt_link *l, uint8_t *p, size_t n) {
    if (l->tls) return tls_recv_some(l, p, n);
    while (l->deadline) {
        int r = wait_readable(l->s, 100);
        if (r < 0 || (r == 0 && expired(l))) return 0;
        if (r > 0) break;
    }
    int k = (int)recv(l->s, (char *)p, (int)n, 0);
    return k > 0 ? k : 0;
}

static int raw_recv(rt_link *l, uint8_t *p, size_t n) {
    while (n) {
        int k = raw_recv_some(l, p, n);
        if (k <= 0) return 0;
        p += k;
        n -= (size_t)k;
    }
    return 1;
}

/* TCP (and TLS for wss://) to u; reads time out after timeout_ms until the caller clears
 * l->deadline. who: the log prefix, NULL for quiet. */
static rt_link *conn_open(const url_parts *u, int timeout_ms, const char *who) {
    sock_t s = tcp_connect(u->host, u->port, timeout_ms);
    if (s == BAD_SOCK) {
        if (who) host_log("%s: cannot connect to %s:%d", who, u->host, u->port);
        return NULL;
    }
    rt_link *l = (rt_link *)calloc(1, sizeof *l);
    l->s = s;
    l->refs = 1;
    l->deadline = (host_time_ms() + (uint32_t)timeout_ms) | 1;
    pthread_mutex_init(&l->send_lock, NULL);
    pthread_mutex_init(&l->tls_lock, NULL);
    int err = u->tls ? tls_start(l, u->host) : 0;
    if (err) {
        if (who) host_log("%s: TLS with %s failed (BearSSL error %d)", who, u->host, err);
        link_release(l);
        return NULL;
    }
    return l;
}

/* conn_open; for wss:// on the standard port, ws:// on port 80 when TLS fails (a device whose
 * clock is wrong cannot check certificates; the relay serves both). */
static rt_link *conn_open_any(url_parts *u, int timeout_ms, const char *who) {
    rt_link *l = conn_open(u, timeout_ms, who);
    if (!l && u->tls && !u->own_port) {
        u->tls = 0;
        u->port = 80;
        l = conn_open(u, timeout_ms, who);
        if (l && who) host_log("%s: using ws://%s (no wss://)", who, u->host);
    }
    return l;
}

rt_link *link_open(const char *url, const char *who) {
    url_parts u;
    if (!parse_url(url, &u)) { host_log("%s: not a relay address: %s", who, url); return NULL; }
    rt_link *l = conn_open_any(&u, CONNECT_TIMEOUT_MS, who);
    if (!l) return NULL;
    char req[600], resp[1024];
    snprintf(req, sizeof req,
             "GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
             "Sec-WebSocket-Key: UmVzZXRHZW5lcmF0aW9uIQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
             u.path, u.host, u.port);
    size_t got = 0;
    int ok = raw_send(l, (const uint8_t *)req, strlen(req));
    while (ok && got < sizeof resp - 1) {
        if (!raw_recv(l, (uint8_t *)resp + got, 1)) break;
        got++;
        if (got >= 4 && !memcmp(resp + got - 4, "\r\n\r\n", 4)) break;
    }
    resp[got] = 0;
    l->deadline = 0;
    if (!ok || !strstr(resp, " 101 ")) {
        host_log("%s: %s refused the WebSocket upgrade", who, url);
        link_release(l);
        return NULL;
    }
    return l;
}

void link_ref(rt_link *l) { __atomic_add_fetch(&l->refs, 1, __ATOMIC_SEQ_CST); }

void link_release(rt_link *l) {
    if (!l || __atomic_sub_fetch(&l->refs, 1, __ATOMIC_SEQ_CST) != 0) return;
    closesock(l->s);
    pthread_mutex_destroy(&l->send_lock);
    pthread_mutex_destroy(&l->tls_lock);
    free(l->tls);
    free(l);
}

static int send_frame(rt_link *l, int opcode, const void *data, size_t len) {
    uint8_t hdr[14], mask[4];
    size_t h = 0;
    uint32_t r = (uint32_t)rand() ^ (uint32_t)time(NULL) * 2654435761u;
    memcpy(mask, &r, 4);
    hdr[h++] = (uint8_t)(0x80 | opcode);
    if (len < 126) hdr[h++] = (uint8_t)(0x80 | len);
    else if (len < 65536) { hdr[h++] = 0x80 | 126; hdr[h++] = (uint8_t)(len >> 8); hdr[h++] = (uint8_t)len; }
    else { hdr[h++] = 0x80 | 127; for (int i = 7; i >= 0; i--) hdr[h++] = (uint8_t)((uint64_t)len >> (i * 8)); }
    memcpy(hdr + h, mask, 4);
    h += 4;
    uint8_t *buf = (uint8_t *)malloc(h + len);
    memcpy(buf, hdr, h);
    for (size_t i = 0; i < len; i++) buf[h + i] = ((const uint8_t *)data)[i] ^ mask[i & 3];
    int ok = !l->closed && raw_send(l, buf, h + len);
    free(buf);
    return ok;
}

int link_send(rt_link *l, int opcode, const void *data, size_t len) {
    if (!l) return 0;
    pthread_mutex_lock(&l->send_lock);
    int ok = send_frame(l, opcode, data, len);
    pthread_mutex_unlock(&l->send_lock);
    return ok;
}

int link_read(rt_link *l, uint8_t *buf, size_t cap, size_t *len) {
    for (;;) {
        uint8_t h[2], mask[4] = {0};
        if (!raw_recv(l, h, 2)) return 0;
        int op = h[0] & 15;
        uint64_t n = h[1] & 127;
        if (n == 126) { uint8_t e[2]; if (!raw_recv(l, e, 2)) return 0; n = (uint64_t)e[0] << 8 | e[1]; }
        else if (n == 127) { uint8_t e[8]; if (!raw_recv(l, e, 8)) return 0; n = 0; for (int i = 0; i < 8; i++) n = n << 8 | e[i]; }
        if ((h[1] & 0x80) && !raw_recv(l, mask, 4)) return 0;
        if (n + 1 > cap) return 0;
        if (!raw_recv(l, buf, (size_t)n)) return 0;
        for (uint64_t i = 0; i < n && (h[1] & 0x80); i++) buf[i] ^= mask[i & 3];
        buf[n] = 0;
        if (op == 8) return 0;
        if (op == 9) { link_send(l, 10, buf, (size_t)n); continue; }
        if (op == 1 || op == 2) { *len = (size_t)n; return op; }
    }
}

void link_close(rt_link *l) {
    if (!l) return;
    pthread_mutex_lock(&l->send_lock);
    if (!l->closed) {
        send_frame(l, 8, "", 0);
        l->closed = 1;
        if (l->tls) {
            pthread_mutex_lock(&l->tls_lock);
            br_ssl_engine_close(&l->tls->cc.eng);
            tls_flush(l);
            pthread_mutex_unlock(&l->tls_lock);
        }
        shutdown(l->s, SHUT_RDWR);
    }
    pthread_mutex_unlock(&l->send_lock);
}

/* ------------------------------------------------------------------ relay choice */
static relay_starter starter;
void rg_set_relay_starter(relay_starter fn) { starter = fn; }

int relay_reachable(const char *url) {
    url_parts u;
    if (!parse_url(url, &u)) return 0;
    rt_link *l = conn_open_any(&u, 2000, NULL);
    if (!l) return 0;
    char req[300], resp[2048];
    snprintf(req, sizeof req, "GET /status HTTP/1.0\r\nHost: %s\r\n\r\n", u.host);
    size_t got = 0;
    if (raw_send(l, (const uint8_t *)req, strlen(req))) {
        int k;
        while (got < sizeof resp - 1 && (k = raw_recv_some(l, (uint8_t *)resp + got, sizeof resp - 1 - got)) > 0) got += (size_t)k;
    }
    resp[got] = 0;
    link_release(l);
    return !strncmp(resp, "HTTP/1.", 7) && strstr(resp, "\"players\"") != NULL;
}

/* broadcast addresses of this device's networks, then the limited broadcast and this device */
static int targets(uint32_t *out, int max) {
    int n = 0;
#ifdef _WIN32
    sock_t s = socket(AF_INET, SOCK_DGRAM, 0);
    INTERFACE_INFO info[32];
    DWORD bytes = 0;
    if (s != BAD_SOCK && WSAIoctl(s, SIO_GET_INTERFACE_LIST, NULL, 0, info, sizeof info, &bytes, NULL, NULL) == 0) {
        for (DWORD i = 0; i < bytes / sizeof info[0] && n < max - 2; i++) {
            if (!(info[i].iiFlags & IFF_UP) || (info[i].iiFlags & IFF_LOOPBACK) || !(info[i].iiFlags & IFF_BROADCAST)) continue;
            uint32_t a = info[i].iiAddress.AddressIn.sin_addr.s_addr, m = info[i].iiNetmask.AddressIn.sin_addr.s_addr;
            out[n++] = a | ~m;
        }
    }
    if (s != BAD_SOCK) closesock(s);
#else
    struct ifaddrs *list = NULL;
    if (getifaddrs(&list) == 0) {
        for (struct ifaddrs *i = list; i && n < max - 2; i = i->ifa_next) {
            if (!i->ifa_addr || i->ifa_addr->sa_family != AF_INET || !(i->ifa_flags & IFF_UP)) continue;
            if ((i->ifa_flags & IFF_LOOPBACK) || !(i->ifa_flags & IFF_BROADCAST) || !i->ifa_broadaddr) continue;
            out[n++] = ((struct sockaddr_in *)i->ifa_broadaddr)->sin_addr.s_addr;
        }
        freeifaddrs(list);
    }
#endif
    out[n++] = htonl(INADDR_BROADCAST);
    out[n++] = htonl(INADDR_LOOPBACK);
    return n;
}

/* Waits up to timeout_ms for a host on the LAN; a host waiting for players wins over a relay that
 * has no game yet (several on one network). 1 with its address in buf. */
static int lan_find(int timeout_ms, char *buf, size_t n) {
    net_startup();
    sock_t s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s == BAD_SOCK) return 0;
    int one = 1;
    setsockopt(s, SOL_SOCKET, SO_BROADCAST, (const char *)&one, sizeof one);
    set_timeout(s, SO_RCVTIMEO, 200);
    uint32_t to[34];
    int nt = targets(to, 34);
    uint32_t start = host_time_ms(), next_probe = start, settle = 0;
    int found = 0;
    for (uint32_t now = start; now - start < (uint32_t)timeout_ms && !(settle && (int32_t)(now - settle) >= 0); now = host_time_ms()) {
        if ((int32_t)(now - next_probe) >= 0) {
            next_probe = now + 1000;
            for (int i = 0; i < nt; i++) {
                struct sockaddr_in a;
                memset(&a, 0, sizeof a);
                a.sin_family = AF_INET;
                a.sin_port = htons(RELAY_PORT);
                a.sin_addr.s_addr = to[i];
                sendto(s, "RGR-DISCOVER 1", 14, 0, (struct sockaddr *)&a, sizeof a);
            }
        }
        char msg[256];
        struct sockaddr_in from;
        socklen_t fl = sizeof from;
        int k = (int)recvfrom(s, msg, sizeof msg - 1, 0, (struct sockaddr *)&from, &fl);
        if (k <= 0) continue;
        msg[k] = 0;
        int port = 0, waiting = 0;
        char who[64] = "?";
        if (sscanf(msg, "RGR-HOST 1 %d %d %63s", &port, &waiting, who) < 2 || port <= 0) continue;
        char ip[64];
        inet_ntop(AF_INET, &from.sin_addr, ip, sizeof ip);
        if (waiting > 0 || !found) {
            snprintf(buf, n, "ws://%s:%d/ws", ip, port);
            found = 1;
            host_log("lan: %s %s at %s", who, waiting > 0 ? "hosts" : "runs a relay (no game waiting yet)", buf);
            if (waiting > 0) break;
            settle = host_time_ms() + 1500;   /* a waiting host may still answer */
        }
    }
    closesock(s);
    return found;
}

static int is_mode(const char *s, const char *mode) {
    if (!s || !*s) return !strcmp(mode, "auto");
    while (*s && *mode && (*s | 32) == *mode) { s++; mode++; }
    return !*s && !*mode;
}

const char *relay_for_joining(const char *setting, char *buf, size_t n) {
    if (is_mode(setting, "online")) return RG_ONLINE_SERVER;
    if (is_mode(setting, "lan")) return lan_find(20000, buf, n) ? buf : NULL;
    if (is_mode(setting, "auto")) return lan_find(2500, buf, n) ? buf : RG_ONLINE_SERVER;
    return setting;
}

const char *relay_for_hosting(const char *setting, const char *nick, char *buf, size_t n) {
    if (is_mode(setting, "online")) return RG_ONLINE_SERVER;
    if (is_mode(setting, "auto") && relay_reachable(RG_ONLINE_SERVER)) return RG_ONLINE_SERVER;
    if (is_mode(setting, "auto") || is_mode(setting, "lan")) {
        const char *u = starter ? starter(nick) : NULL;
        if (!u) return NULL;
        snprintf(buf, n, "%s", u);
        return buf;
    }
    return setting;
}
