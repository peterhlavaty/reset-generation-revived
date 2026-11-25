/* stdio.h over the in-memory file system (fs.c); stdout and stderr go to the browser console
 * line by line. */
#include "fs.h"
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void rg_web_log(const char *line, size_t len);   /* Rust host: console.log */

struct rg_file {
    fs_node *node;          /* NULL for stdout / stderr */
    size_t pos;
    int readable, writable, append, eof, dirty;
    char line[512];
    size_t linelen;
};

static struct rg_file out_file, err_file;
FILE *const stdout = &out_file, *const stderr = &err_file;

FILE *fopen(const char *path, const char *mode) {
    char norm[1024];
    if (!fs_normalize(path, norm, sizeof norm)) { errno = ENOENT; return NULL; }
    int plus = strchr(mode, '+') != NULL;
    fs_node *n = fs_find(norm);
    if (n && n->dir) { errno = EINVAL; return NULL; }
    if (mode[0] == 'r') {
        if (!n) { errno = ENOENT; return NULL; }
    } else {
        if (!n && !(n = fs_create(norm, 0))) { errno = ENOMEM; return NULL; }
        if (mode[0] == 'w') n->size = 0;
    }
    FILE *f = calloc(1, sizeof *f);
    if (!f) { errno = ENOMEM; return NULL; }
    f->node = n;
    f->readable = mode[0] == 'r' || plus;
    f->writable = mode[0] != 'r' || plus;
    f->append = mode[0] == 'a';
    f->dirty = mode[0] == 'w';   /* a truncated file has changed even if nothing is written */
    return f;
}

static void flush_console(FILE *f) {
    if (f->linelen) rg_web_log(f->line, f->linelen);
    f->linelen = 0;
}

int fflush(FILE *f) {
    if (!f) return 0;
    if (!f->node) flush_console(f);
    else if (f->dirty) { fs_changed(f->node); f->dirty = 0; }
    return 0;
}

int fclose(FILE *f) {
    fflush(f);
    if (f->node) free(f);
    return 0;
}

size_t fread(void *p, size_t size, size_t n, FILE *f) {
    if (!f->node || !f->readable || !size) return 0;
    size_t avail = f->pos < f->node->size ? f->node->size - f->pos : 0;
    size_t items = avail / size < n ? avail / size : n;
    if (items < n) f->eof = 1;
    memcpy(p, f->node->data + f->pos, items * size);
    f->pos += items * size;
    return items;
}

size_t fwrite(const void *p, size_t size, size_t n, FILE *f) {
    size_t len = size * n;
    if (!f->node) {   /* console */
        const char *s = p;
        for (size_t i = 0; i < len; i++) {
            if (s[i] == '\n') flush_console(f);
            else {
                if (f->linelen == sizeof f->line) flush_console(f);
                f->line[f->linelen++] = s[i];
            }
        }
        return n;
    }
    if (!f->writable || !len) return 0;
    fs_node *d = f->node;
    if (f->append) f->pos = d->size;
    if (!fs_reserve(d, f->pos + len)) { errno = ENOMEM; return 0; }
    if (f->pos > d->size) memset(d->data + d->size, 0, f->pos - d->size);
    memcpy(d->data + f->pos, p, len);
    f->pos += len;
    if (f->pos > d->size) d->size = f->pos;
    f->dirty = 1;
    return n;
}

int fseek(FILE *f, long off, int whence) {
    if (!f->node) return -1;
    long base = whence == SEEK_SET ? 0 : whence == SEEK_CUR ? (long)f->pos : (long)f->node->size;
    if (base + off < 0) { errno = EINVAL; return -1; }
    f->pos = (size_t)(base + off);
    f->eof = 0;
    return 0;
}
long ftell(FILE *f) { return f->node ? (long)f->pos : -1; }
int feof(FILE *f) { return f->eof; }

int fgetc(FILE *f) {
    unsigned char c;
    return fread(&c, 1, 1, f) == 1 ? c : EOF;
}
char *fgets(char *s, int n, FILE *f) {
    int i = 0;
    while (i < n - 1) {
        int c = fgetc(f);
        if (c == EOF) break;
        s[i++] = (char)c;
        if (c == '\n') break;
    }
    if (!i) return NULL;
    s[i] = 0;
    return s;
}
int fputc(int c, FILE *f) {
    unsigned char b = (unsigned char)c;
    return fwrite(&b, 1, 1, f) == 1 ? b : EOF;
}
int fputs(const char *s, FILE *f) { return fwrite(s, 1, strlen(s), f) ? 0 : EOF; }
int puts(const char *s) { fputs(s, stdout); return fputc('\n', stdout); }

/* ------------------------------------------------------------------ formatting */
typedef struct { char *buf; size_t cap, len; } sink;

static void put(sink *o, const char *s, size_t n) {
    for (size_t i = 0; i < n; i++, o->len++)
        if (o->len + 1 < o->cap) o->buf[o->len] = s[i];
}
static void pad(sink *o, char c, int n) {
    for (; n > 0; n--) put(o, &c, 1);
}

int vsnprintf(char *buf, size_t cap, const char *fmt, va_list ap) {
    sink o = {buf, cap, 0};
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') { put(&o, p, 1); continue; }
        p++;
        int left = 0, zero = 0, plus = 0, space = 0, alt = 0;
        for (;; p++) {
            if (*p == '-') left = 1;
            else if (*p == '0') zero = 1;
            else if (*p == '+') plus = 1;
            else if (*p == ' ') space = 1;
            else if (*p == '#') alt = 1;
            else break;
        }
        int width = 0, prec = -1;
        if (*p == '*') { width = va_arg(ap, int); if (width < 0) left = 1, width = -width; p++; }
        else while (isdigit((unsigned char)*p)) width = width * 10 + (*p++ - '0');
        if (*p == '.') {
            p++;
            prec = 0;
            if (*p == '*') { prec = va_arg(ap, int); p++; }
            else while (isdigit((unsigned char)*p)) prec = prec * 10 + (*p++ - '0');
        }
        int lng = 0;   /* 1 l, 2 ll, -1 h, -2 hh */
        for (;; p++) {
            if (*p == 'l') lng++;
            else if (*p == 'h') lng--;
            else if (*p == 'z' || *p == 'j' || *p == 't') lng = 2;
            else break;
        }
        char tmp[72], *s = tmp;
        size_t n = 0;
        const char *prefix = "";
        switch (*p) {
        case 'd': case 'i': {
            long long v = lng >= 2 ? va_arg(ap, long long) : lng == 1 ? va_arg(ap, long) : va_arg(ap, int);
            if (lng == -1) v = (short)v;
            if (lng == -2) v = (signed char)v;
            unsigned long long u = v < 0 ? 0ull - (unsigned long long)v : (unsigned long long)v;
            char *e = tmp + sizeof tmp;
            s = e;
            do *--s = (char)('0' + u % 10); while (u /= 10);
            if (prec == 0 && v == 0) s = e;
            n = (size_t)(e - s);
            prefix = v < 0 ? "-" : plus ? "+" : space ? " " : "";
            break;
        }
        case 'u': case 'x': case 'X': case 'o': case 'p': {
            unsigned long long u;
            if (*p == 'p') u = (uintptr_t)va_arg(ap, void *), alt = 1;
            else u = lng >= 2 ? va_arg(ap, unsigned long long) : lng == 1 ? va_arg(ap, unsigned long) : va_arg(ap, unsigned);
            if (lng == -1) u = (unsigned short)u;
            if (lng == -2) u = (unsigned char)u;
            unsigned base = *p == 'o' ? 8 : *p == 'u' ? 10 : 16;
            const char *dig = *p == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
            char *e = tmp + sizeof tmp;
            s = e;
            int was_zero = u == 0;
            do *--s = dig[u % base]; while (u /= base);
            if (prec == 0 && was_zero) s = e;
            n = (size_t)(e - s);
            if (alt && !was_zero && base == 16) prefix = *p == 'X' ? "0X" : "0x";
            if (alt && base == 8 && *s != '0') *--s = '0', n++;
            break;
        }
        case 'f': case 'F': case 'g': case 'G': case 'e': case 'E': {
            /* plain fixed point (only debug output prints doubles) */
            double v = va_arg(ap, double);
            if (prec < 0) prec = 6;
            if (prec > 17) prec = 17;
            if (v < 0) prefix = "-", v = -v;
            else if (plus) prefix = "+";
            if (v != v) { s = "nan"; n = 3; break; }
            if (v > 1e18) { s = "inf"; n = 3; break; }
            double scale = 1;
            for (int i = 0; i < prec; i++) scale *= 10;
            unsigned long long whole = (unsigned long long)v;
            unsigned long long frac = (unsigned long long)((v - (double)whole) * scale + 0.5);
            if ((double)frac >= scale) whole++, frac = 0;
            char *e = tmp + sizeof tmp;
            s = e;
            for (int i = 0; i < prec; i++) *--s = (char)('0' + frac % 10), frac /= 10;
            if (prec) *--s = '.';
            do *--s = (char)('0' + whole % 10); while (whole /= 10);
            n = (size_t)(e - s);
            prec = -1;
            break;
        }
        case 'c': tmp[0] = (char)va_arg(ap, int); n = 1; break;
        case 's':
            s = va_arg(ap, char *);
            if (!s) s = "(null)";
            n = prec >= 0 ? strnlen(s, (size_t)prec) : strlen(s);
            prec = -1;
            break;
        case '%': tmp[0] = '%'; n = 1; break;
        case 0: p--; continue;
        default: put(&o, p - 1, 2); continue;
        }
        int digits = (prec >= 0 && (size_t)prec > n) ? prec - (int)n : 0;
        int total = (int)(strlen(prefix) + n) + digits;
        if (prec >= 0) zero = 0;
        if (!left && !zero) pad(&o, ' ', width - total);
        put(&o, prefix, strlen(prefix));
        if (!left && zero) pad(&o, '0', width - total);
        pad(&o, '0', digits);
        put(&o, s, n);
        if (left) pad(&o, ' ', width - total);
    }
    if (cap) buf[o.len < cap ? o.len : cap - 1] = 0;
    return (int)o.len;
}

int snprintf(char *s, size_t n, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(s, n, fmt, ap);
    va_end(ap);
    return r;
}
int sprintf(char *s, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(s, (size_t)-1 / 2, fmt, ap);
    va_end(ap);
    return r;
}
int vfprintf(FILE *f, const char *fmt, va_list ap) {
    char small[512];
    va_list ap2;
    va_copy(ap2, ap);
    int n = vsnprintf(small, sizeof small, fmt, ap);
    if (n < 0) { va_end(ap2); return n; }
    if ((size_t)n < sizeof small) fwrite(small, 1, (size_t)n, f);
    else {
        char *big = malloc((size_t)n + 1);
        if (big) {
            vsnprintf(big, (size_t)n + 1, fmt, ap2);
            fwrite(big, 1, (size_t)n, f);
            free(big);
        }
    }
    va_end(ap2);
    return n;
}
int fprintf(FILE *f, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(f, fmt, ap);
    va_end(ap);
    return r;
}
int printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(stdout, fmt, ap);
    va_end(ap);
    return r;
}

/* %d %u %x %s %c with optional widths; enough for the runtime's debug settings */
int sscanf(const char *s, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int got = 0;
    for (const char *p = fmt; *p; p++) {
        if (isspace((unsigned char)*p)) { while (isspace((unsigned char)*s)) s++; continue; }
        if (*p != '%') { if (*s != *p) break; s++; continue; }
        p++;
        while (*p == 'l' || *p == 'h') p++;
        if (*p == 'd' || *p == 'u' || *p == 'x' || *p == 'i') {
            char *end;
            long long v = *p == 'x' ? (long long)strtoull(s, &end, 16) : strtoll(s, &end, *p == 'i' ? 0 : 10);
            if (end == s) break;
            *va_arg(ap, int *) = (int)v;
            s = end;
            got++;
        } else if (*p == 's') {
            while (isspace((unsigned char)*s)) s++;
            if (!*s) break;
            char *d = va_arg(ap, char *);
            while (*s && !isspace((unsigned char)*s)) *d++ = *s++;
            *d = 0;
            got++;
        } else if (*p == 'c') {
            if (!*s) break;
            *va_arg(ap, char *) = *s++;
            got++;
        } else break;
    }
    va_end(ap);
    return got;
}
