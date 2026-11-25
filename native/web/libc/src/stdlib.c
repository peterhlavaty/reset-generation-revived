/* stdlib.h (malloc and friends are in the Rust host), errno, the single-threaded pthread
 * stand-ins, time.h and unistd.h */
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>
#include <pthread.h>
#include <time.h>
#include <unistd.h>

/* ------------------------------------------------------------------ from the Rust host */
_Noreturn void rg_web_abort(const char *why);
double rg_web_now_ms(void);      /* performance.now() */
double rg_web_epoch_ms(void);    /* Date.now() */

int errno;

_Noreturn void abort(void) { rg_web_abort("abort()"); }
_Noreturn void exit(int code) { rg_web_abort(code ? "exit(failure)" : "exit(0)"); }
_Noreturn void __rg_assert_fail(const char *expr, const char *file, int line) {
    (void)file;
    (void)line;
    rg_web_abort(expr);
}

char *getenv(const char *name) { (void)name; return NULL; }

char *realpath(const char *path, char *out) {
    if (path[0] == '/') strcpy(out, path);
    else if (!strcmp(path, ".")) strcpy(out, "/");
    else { out[0] = '/'; strcpy(out + 1, path); }
    return out;
}

static unsigned long long to_ull(const char *s, char **end, int base, int *neg) {
    const char *p = s;
    while (isspace((unsigned char)*p)) p++;
    *neg = 0;
    if (*p == '+' || *p == '-') *neg = *p++ == '-';
    if ((base == 0 || base == 16) && p[0] == '0' && (p[1] | 32) == 'x' && isxdigit((unsigned char)p[2])) p += 2, base = 16;
    else if (base == 0) base = p[0] == '0' ? 8 : 10;
    unsigned long long v = 0;
    const char *digits = p;
    for (;; p++) {
        int c = (unsigned char)*p, d;
        if (isdigit(c)) d = c - '0';
        else if (isalpha(c)) d = tolower(c) - 'a' + 10;
        else break;
        if (d >= base) break;
        v = v * (unsigned)base + (unsigned)d;
    }
    if (end) *end = (char *)(p == digits ? s : p);
    return v;
}
long long strtoll(const char *s, char **end, int base) {
    int neg;
    unsigned long long v = to_ull(s, end, base, &neg);
    return neg ? -(long long)v : (long long)v;
}
unsigned long long strtoull(const char *s, char **end, int base) {
    int neg;
    unsigned long long v = to_ull(s, end, base, &neg);
    return neg ? -v : v;
}
long strtol(const char *s, char **end, int base) { return (long)strtoll(s, end, base); }
unsigned long strtoul(const char *s, char **end, int base) { return (unsigned long)strtoull(s, end, base); }
int atoi(const char *s) { return (int)strtol(s, NULL, 10); }
long atol(const char *s) { return strtol(s, NULL, 10); }

static void swap(unsigned char *a, unsigned char *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        unsigned char t = a[i];
        a[i] = b[i];
        b[i] = t;
    }
}
static void sift(unsigned char *b, size_t root, size_t n, size_t size, int (*cmp)(const void *, const void *)) {
    for (;;) {
        size_t child = 2 * root + 1;
        if (child >= n) return;
        if (child + 1 < n && cmp(b + child * size, b + (child + 1) * size) < 0) child++;
        if (cmp(b + root * size, b + child * size) >= 0) return;
        swap(b + root * size, b + child * size, size);
        root = child;
    }
}
void qsort(void *base, size_t n, size_t size, int (*cmp)(const void *, const void *)) {
    unsigned char *b = base;
    for (size_t i = n / 2; i-- > 0;) sift(b, i, n, size, cmp);
    for (size_t end = n; end-- > 1;) {
        swap(b, b + end * size, size);
        sift(b, 0, end, size, cmp);
    }
}

static unsigned long long rand_state = 1;
void srand(unsigned seed) { rand_state = seed; }
int rand(void) {
    rand_state = rand_state * 6364136223846793005ull + 1442695040888963407ull;
    return (int)(rand_state >> 33);
}

/* ------------------------------------------------------------------ one thread */
int pthread_create(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg) {
    (void)t, (void)a, (void)fn, (void)arg;
    return EAGAIN;
}
int pthread_detach(pthread_t t) { (void)t; return 0; }
int pthread_once(pthread_once_t *o, void (*fn)(void)) {
    if (!*o) { *o = 1; fn(); }
    return 0;
}
int pthread_mutex_init(pthread_mutex_t *m, const pthread_mutexattr_t *a) { (void)m, (void)a; return 0; }
int pthread_mutex_destroy(pthread_mutex_t *m) { (void)m; return 0; }
int pthread_mutex_lock(pthread_mutex_t *m) { (void)m; return 0; }
int pthread_mutex_unlock(pthread_mutex_t *m) { (void)m; return 0; }
int pthread_cond_init(pthread_cond_t *c, const pthread_condattr_t *a) { (void)c, (void)a; return 0; }
int pthread_cond_destroy(pthread_cond_t *c) { (void)c; return 0; }
/* nobody else could signal: waiting would never end */
int pthread_cond_wait(pthread_cond_t *c, pthread_mutex_t *m) { (void)c, (void)m; rg_web_abort("pthread_cond_wait with one thread"); }
int pthread_cond_timedwait(pthread_cond_t *c, pthread_mutex_t *m, const struct timespec *t) { (void)c, (void)m, (void)t; return ETIMEDOUT; }
int pthread_cond_signal(pthread_cond_t *c) { (void)c; return 0; }
int pthread_cond_broadcast(pthread_cond_t *c) { (void)c; return 0; }

/* ------------------------------------------------------------------ time */
int clock_gettime(clockid_t id, struct timespec *t) {
    double ms = id == CLOCK_REALTIME ? rg_web_epoch_ms() : rg_web_now_ms();
    long long us = (long long)(ms * 1000.0);
    t->tv_sec = us / 1000000;
    t->tv_nsec = (long)(us % 1000000) * 1000;
    return 0;
}
time_t time(time_t *t) {
    time_t v = (time_t)(rg_web_epoch_ms() / 1000.0);
    if (t) *t = v;
    return v;
}
struct tm *gmtime(const time_t *t) {
    static struct tm r;
    long long s = *t, days = s / 86400, rem = s % 86400;
    if (rem < 0) rem += 86400, days--;
    r.tm_hour = (int)(rem / 3600);
    r.tm_min = (int)(rem / 60 % 60);
    r.tm_sec = (int)(rem % 60);
    r.tm_wday = (int)((days + 4) % 7 + 7) % 7;
    /* civil from days (H. Hinnant) */
    long long z = days + 719468, era = (z >= 0 ? z : z - 146096) / 146097;
    long long doe = z - era * 146097, yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    long long y = yoe + era * 400, doy = doe - (365 * yoe + yoe / 4 - yoe / 100), mp = (5 * doy + 2) / 153;
    int d = (int)(doy - (153 * mp + 2) / 5 + 1), m = (int)(mp < 10 ? mp + 3 : mp - 9);
    y += m <= 2;
    r.tm_year = (int)(y - 1900);
    r.tm_mon = m - 1;
    r.tm_mday = d;
    static const short before[12] = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334};
    int leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    r.tm_yday = before[m - 1] + d - 1 + (leap && m > 2);
    r.tm_isdst = 0;
    return &r;
}
struct tm *localtime(const time_t *t) { return gmtime(t); }

/* nothing else runs while the engine waits: a sleep returns at once (the engine's own Sleep()
 * goes through the virtual clock, native/web/src/lib.rs) */
int usleep(unsigned us) { (void)us; return 0; }
unsigned sleep(unsigned s) { (void)s; return 0; }
