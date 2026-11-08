/* Check-only stand-in (aarch64-apple-darwin); see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_MATH_H
#define SA_CHECK_MATH_H
#define INFINITY (__builtin_inff())
#define NAN (__builtin_nanf(""))
#endif
