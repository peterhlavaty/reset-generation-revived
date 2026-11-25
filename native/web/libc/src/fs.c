/* The in-memory file system (fs.h), dirent.h and sys/stat.h. */
#include "fs.h"
#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

/* from the Rust host: a file the engine wrote (or removed: data NULL), to keep between visits */
void rg_web_file_changed(const char *path, const uint8_t *data, size_t len);

static fs_node **nodes;
static size_t count, cap;

int fs_normalize(const char *path, char *out, size_t n) {
    size_t o = 0;
    out[o++] = '/';
    for (const char *p = path; *p;) {
        while (*p == '/' || *p == '\\') p++;
        const char *s = p;
        while (*p && *p != '/' && *p != '\\') p++;
        size_t l = (size_t)(p - s);
        if (!l || (l == 1 && s[0] == '.')) continue;
        if (l == 2 && s[0] == '.' && s[1] == '.') {
            while (o > 1 && out[o - 1] != '/') o--;
            if (o > 1) o--;
            continue;
        }
        if (o > 1) out[o++] = '/';
        if (o + l + 1 > n) return 0;
        memcpy(out + o, s, l);
        o += l;
    }
    out[o] = 0;
    return 1;
}

fs_node *fs_find(const char *norm) {
    for (size_t i = 0; i < count; i++)
        if (!strcasecmp(nodes[i]->path, norm)) return nodes[i];
    return NULL;
}

fs_node *fs_create(const char *norm, int dir) {
    fs_node *f = fs_find(norm);
    if (f) return f;
    if (strcmp(norm, "/")) {   /* the parent folders */
        char parent[1024];
        snprintf(parent, sizeof parent, "%s", norm);
        char *slash = strrchr(parent, '/');
        if (slash == parent) slash[1] = 0;
        else *slash = 0;
        if (!fs_create(parent, 1)) return NULL;
    }
    if (count == cap) {
        size_t c = cap ? cap * 2 : 64;
        fs_node **n = realloc(nodes, c * sizeof *n);
        if (!n) return NULL;
        nodes = n;
        cap = c;
    }
    f = calloc(1, sizeof *f);
    if (!f || !(f->path = strdup(norm))) { free(f); return NULL; }
    f->dir = dir;
    nodes[count++] = f;
    return f;
}

int fs_reserve(fs_node *f, size_t size) {
    if (size <= f->cap) return 1;
    size_t c = f->cap ? f->cap : 256;
    while (c < size) c *= 2;
    uint8_t *d = realloc(f->data, c);
    if (!d) return 0;
    f->data = d;
    f->cap = c;
    return 1;
}

void fs_changed(fs_node *f) { rg_web_file_changed(f->path, f->data, f->size); }

static void unlink_node(size_t i) {
    free(nodes[i]->path);
    free(nodes[i]->data);
    free(nodes[i]);
    nodes[i] = nodes[--count];
}

/* The host adds a file (a copy of data); 0 if out of memory. */
int rg_fs_add(const char *path, const uint8_t *data, size_t len) {
    char norm[1024];
    if (!fs_normalize(path, norm, sizeof norm)) return 0;
    fs_node *f = fs_create(norm, 0);
    if (!f || f->dir || !fs_reserve(f, len)) return 0;
    memcpy(f->data, data, len);
    f->size = len;
    return 1;
}

int remove(const char *path) {
    char norm[1024];
    if (!fs_normalize(path, norm, sizeof norm)) return -1;
    for (size_t i = 0; i < count; i++) {
        if (!strcasecmp(nodes[i]->path, norm) && !nodes[i]->dir) {
            rg_web_file_changed(nodes[i]->path, NULL, 0);
            unlink_node(i);
            return 0;
        }
    }
    errno = ENOENT;
    return -1;
}

int rename(const char *from, const char *to) {
    char a[1024], b[1024];
    if (!fs_normalize(from, a, sizeof a) || !fs_normalize(to, b, sizeof b)) return -1;
    fs_node *f = fs_find(a);
    if (!f || f->dir) { errno = ENOENT; return -1; }
    fs_node *t = fs_create(b, 0);
    if (!t || t->dir) return -1;
    if (t == f) return 0;
    free(t->data);
    t->data = f->data, t->size = f->size, t->cap = f->cap;
    f->data = NULL, f->size = f->cap = 0;
    fs_changed(t);
    remove(a);
    return 0;
}

int stat(const char *path, struct stat *st) {
    char norm[1024];
    fs_node *f = fs_normalize(path, norm, sizeof norm) ? fs_find(norm) : NULL;
    if (!f) { errno = ENOENT; return -1; }
    memset(st, 0, sizeof *st);
    st->st_mode = f->dir ? S_IFDIR | 0755 : S_IFREG | 0644;
    st->st_size = (long long)f->size;
    return 0;
}

int mkdir(const char *path, mode_t mode) {
    (void)mode;
    char norm[1024];
    if (!fs_normalize(path, norm, sizeof norm)) return -1;
    if (fs_find(norm)) { errno = EEXIST; return -1; }
    return fs_create(norm, 1) ? 0 : -1;
}

struct rg_dir {
    char path[1024];
    size_t next;
    struct dirent entry;
};

DIR *opendir(const char *path) {
    DIR *d = calloc(1, sizeof *d);
    if (!d) return NULL;
    fs_node *f = fs_normalize(path, d->path, sizeof d->path) ? fs_find(d->path) : NULL;
    if (!f || !f->dir) { free(d); errno = ENOENT; return NULL; }
    return d;
}

struct dirent *readdir(DIR *d) {
    size_t l = strlen(d->path);
    while (d->next < count) {
        const char *p = nodes[d->next++]->path;
        /* direct children only: "<dir>/<name>" with no further '/' */
        if (l == 1) { if (p[0] != '/' || !p[1]) continue; p += 1; }
        else { if (strncasecmp(p, d->path, l) || p[l] != '/') continue; p += l + 1; }
        if (strchr(p, '/')) continue;
        snprintf(d->entry.d_name, sizeof d->entry.d_name, "%s", p);
        return &d->entry;
    }
    return NULL;
}

int closedir(DIR *d) { free(d); return 0; }
