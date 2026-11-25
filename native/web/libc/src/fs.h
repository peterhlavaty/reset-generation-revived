/* The in-memory file system of the browser build (fs.c): the host puts the game files in before
 * the engine starts; files the engine writes are handed back to the host to keep (saves). */
#pragma once
#include <stddef.h>
#include <stdint.h>

typedef struct {
    char *path;            /* normalized: absolute, '/' separated, no "." or ".." */
    uint8_t *data;
    size_t size, cap;
    int dir;
} fs_node;

int fs_normalize(const char *path, char *out, size_t n);   /* 0 if too long */
fs_node *fs_find(const char *norm);                        /* names compare case-insensitively */
fs_node *fs_create(const char *norm, int dir);             /* also creates the parent folders */
int fs_reserve(fs_node *f, size_t size);                   /* grows f->data to hold size bytes */
void fs_changed(fs_node *f);                               /* written and closed: tell the host */
