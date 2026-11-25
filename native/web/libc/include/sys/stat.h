#pragma once
#include "../_rg_libc.h"
typedef unsigned mode_t;
struct stat { mode_t st_mode; long long st_size; };
#define S_IFMT 0170000
#define S_IFDIR 0040000
#define S_IFREG 0100000
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
int stat(const char *path, struct stat *st);
int mkdir(const char *path, mode_t mode);
