#pragma once
#define NAN __builtin_nan("")
#define INFINITY __builtin_inf()
#define HUGE_VAL __builtin_huge_val()
#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define isfinite(x) __builtin_isfinite(x)
#define signbit(x) __builtin_signbit(x)
/* single wasm instructions */
static inline double sqrt(double x) { return __builtin_sqrt(x); }
static inline float sqrtf(float x) { return __builtin_sqrtf(x); }
static inline double fabs(double x) { return __builtin_fabs(x); }
static inline float fabsf(float x) { return __builtin_fabsf(x); }
static inline double floor(double x) { return __builtin_floor(x); }
static inline double ceil(double x) { return __builtin_ceil(x); }
static inline double trunc(double x) { return __builtin_trunc(x); }
static inline double rint(double x) { return __builtin_rint(x); }   /* half to even, like x87 */
static inline double nearbyint(double x) { return __builtin_nearbyint(x); }
static inline double copysign(double x, double y) { return __builtin_copysign(x, y); }
/* from the Rust host (the libm crate) */
double sin(double x);
double cos(double x);
double tan(double x);
double atan(double x);
double atan2(double y, double x);
double exp(double x);
double log(double x);
double pow(double x, double y);
double fmod(double x, double y);
double ldexp(double x, int e);
double frexp(double x, int *e);
