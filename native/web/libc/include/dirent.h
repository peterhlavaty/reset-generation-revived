#pragma once
#include "_rg_libc.h"
typedef struct rg_dir DIR;
struct dirent { char d_name[256]; };
DIR *opendir(const char *path);
struct dirent *readdir(DIR *d);
int closedir(DIR *d);
