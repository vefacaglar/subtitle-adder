/* Check-only stand-in: the Darwin <sys/errno.h> values FFmpeg's error macros refer to; see apps/desktop/scripts/check-macos.sh. */
#ifndef SA_CHECK_ERRNO_H
#define SA_CHECK_ERRNO_H
#define EPERM 1
#define ENOENT 2
#define EINTR 4
#define EIO 5
#define ENOMEM 12
#define EACCES 13
#define EEXIST 17
#define EINVAL 22
#define EPIPE 32
#define EDOM 33
#define ERANGE 34
#define EAGAIN 35
#define ENOSYS 78
#define ETIMEDOUT 60
#endif
