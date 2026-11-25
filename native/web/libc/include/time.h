#pragma once
#include "_rg_libc.h"
typedef long long time_t;
typedef int clockid_t;
struct timespec { time_t tv_sec; long tv_nsec; };
struct tm { int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst; };
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
int clock_gettime(clockid_t id, struct timespec *t);
time_t time(time_t *t);
struct tm *gmtime(const time_t *t);
struct tm *localtime(const time_t *t);   /* UTC (the browser's zone is not looked up) */
