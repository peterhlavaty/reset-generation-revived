#pragma once
_Noreturn void __rg_assert_fail(const char *expr, const char *file, int line);
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
#define assert(e) ((e) ? (void)0 : __rg_assert_fail(#e, __FILE__, __LINE__))
#endif
