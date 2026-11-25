#pragma once
typedef void (*sighandler_t)(int);
#define SIG_DFL ((sighandler_t)0)
#define SIG_IGN ((sighandler_t)1)
#define SIGPIPE 13
static inline sighandler_t signal(int sig, sighandler_t h) { (void)sig; (void)h; return SIG_DFL; }
