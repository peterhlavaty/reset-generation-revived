/* string.h, strings.h */
#include <string.h>
#include <strings.h>
#include <stdlib.h>
#include <ctype.h>

/* memory.copy / memory.fill (bulk memory): the builtins do not call back into these */
void *memcpy(void *restrict d, const void *restrict s, size_t n) { __builtin_memcpy(d, s, n); return d; }
void *memmove(void *d, const void *s, size_t n) { __builtin_memmove(d, s, n); return d; }
void *memset(void *d, int c, size_t n) { __builtin_memset(d, c, n); return d; }

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; i++)
        if (x[i] != y[i]) return x[i] - y[i];
    return 0;
}
void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    for (size_t i = 0; i < n; i++)
        if (p[i] == (unsigned char)c) return (void *)(p + i);
    return NULL;
}
size_t strlen(const char *s) {
    const char *p = s;
    while (*p) p++;
    return (size_t)(p - s);
}
size_t strnlen(const char *s, size_t n) {
    size_t i = 0;
    while (i < n && s[i]) i++;
    return i;
}
int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}
int strncmp(const char *a, const char *b, size_t n) {
    for (; n; n--, a++, b++) {
        if (*a != *b || !*a) return (unsigned char)*a - (unsigned char)*b;
    }
    return 0;
}
int strcasecmp(const char *a, const char *b) {
    while (*a && tolower((unsigned char)*a) == tolower((unsigned char)*b)) a++, b++;
    return tolower((unsigned char)*a) - tolower((unsigned char)*b);
}
int strncasecmp(const char *a, const char *b, size_t n) {
    for (; n; n--, a++, b++) {
        int x = tolower((unsigned char)*a), y = tolower((unsigned char)*b);
        if (x != y || !x) return x - y;
    }
    return 0;
}
char *strcpy(char *restrict d, const char *restrict s) {
    char *r = d;
    while ((*d++ = *s++)) {}
    return r;
}
char *strncpy(char *restrict d, const char *restrict s, size_t n) {
    size_t i = 0;
    for (; i < n && s[i]; i++) d[i] = s[i];
    for (; i < n; i++) d[i] = 0;
    return d;
}
char *strcat(char *restrict d, const char *restrict s) {
    strcpy(d + strlen(d), s);
    return d;
}
char *strncat(char *restrict d, const char *restrict s, size_t n) {
    char *p = d + strlen(d);
    size_t i = 0;
    for (; i < n && s[i]; i++) p[i] = s[i];
    p[i] = 0;
    return d;
}
char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c) return (char *)s;
        if (!*s) return NULL;
    }
}
char *strrchr(const char *s, int c) {
    const char *r = NULL;
    for (;; s++) {
        if (*s == (char)c) r = s;
        if (!*s) return (char *)r;
    }
}
char *strstr(const char *h, const char *n) {
    size_t l = strlen(n);
    if (!l) return (char *)h;
    for (; *h; h++)
        if (*h == *n && !strncmp(h, n, l)) return (char *)h;
    return NULL;
}
char *strdup(const char *s) {
    size_t n = strlen(s) + 1;
    char *d = malloc(n);
    if (d) memcpy(d, s, n);
    return d;
}
char *strerror(int e) {
    switch (e) {
    case 0: return "no error";
    case 2: return "no such file or directory";
    case 12: return "out of memory";
    case 38: return "not supported in the browser";
    default: return "error";
    }
}
