#ifndef _SYS_STAT_H
#define _SYS_STAT_H

/* Minimal stat: the fields the ninep NP_STAT record actually carries (size and
 * the POSIX mode). Grows toward a full struct stat as file I/O matures. */
struct stat {
    long st_size;
    unsigned st_mode;
};

int fstat(int fd, struct stat *st);

#endif
