#pragma once
/* The browser build has one thread: pthread_create fails (the online code checks for that),
 * locks are no-ops and waits return at once. */
#include <time.h>
typedef unsigned long pthread_t;
typedef struct { int unused; } pthread_attr_t;
typedef struct { int unused; } pthread_mutexattr_t;
typedef struct { int unused; } pthread_condattr_t;
typedef struct { int unused; } pthread_mutex_t;
typedef struct { int unused; } pthread_cond_t;
typedef int pthread_once_t;
#define PTHREAD_MUTEX_INITIALIZER {0}
#define PTHREAD_COND_INITIALIZER {0}
#define PTHREAD_ONCE_INIT 0
int pthread_create(pthread_t *t, const pthread_attr_t *a, void *(*fn)(void *), void *arg);
int pthread_detach(pthread_t t);
int pthread_once(pthread_once_t *o, void (*fn)(void));
int pthread_mutex_init(pthread_mutex_t *m, const pthread_mutexattr_t *a);
int pthread_mutex_destroy(pthread_mutex_t *m);
int pthread_mutex_lock(pthread_mutex_t *m);
int pthread_mutex_unlock(pthread_mutex_t *m);
int pthread_cond_init(pthread_cond_t *c, const pthread_condattr_t *a);
int pthread_cond_destroy(pthread_cond_t *c);
int pthread_cond_wait(pthread_cond_t *c, pthread_mutex_t *m);
int pthread_cond_timedwait(pthread_cond_t *c, pthread_mutex_t *m, const struct timespec *t);
int pthread_cond_signal(pthread_cond_t *c);
int pthread_cond_broadcast(pthread_cond_t *c);
