/* Check-only stand-in (aarch64-apple-darwin); see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_STDIO_H
#define SA_CHECK_STDIO_H
#include <stddef.h>
#include <stdarg.h>
typedef struct __sFILE FILE;
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
#define EOF (-1)
#endif
