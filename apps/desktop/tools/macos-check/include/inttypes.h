/* Check-only stand-in (aarch64-apple-darwin); see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_INTTYPES_H
#define SA_CHECK_INTTYPES_H
#include <stdint.h>
#define PRId64 "lld"
#define PRIu64 "llu"
#define PRIx64 "llx"
#define PRIX64 "llX"
#define PRId32 "d"
#define PRIu32 "u"
#define PRIx32 "x"
#endif
