/* Check-only stand-in (aarch64-apple-darwin); see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_STDDEF_H
#define SA_CHECK_STDDEF_H
typedef __SIZE_TYPE__ size_t;
typedef __PTRDIFF_TYPE__ ptrdiff_t;
typedef __WCHAR_TYPE__ wchar_t;
#define NULL ((void *)0)
#define offsetof(t, m) __builtin_offsetof(t, m)
#endif
