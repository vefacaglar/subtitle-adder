/* Check-only stand-in: Darwin's `struct tm` layout (FFmpeg's parseutils.h uses it); see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_TIME_H
#define SA_CHECK_TIME_H
#include <stddef.h>
typedef long time_t;
struct tm {
    int tm_sec;
    int tm_min;
    int tm_hour;
    int tm_mday;
    int tm_mon;
    int tm_year;
    int tm_wday;
    int tm_yday;
    int tm_isdst;
    long tm_gmtoff;
    char *tm_zone;
};
#endif
