#ifndef _SYS_STAT_H
#define _SYS_STAT_H

/* Minimal stat: the fields the ninep NP_STAT record carries (size, and the
 * POSIX mode, uid and gid where the filesystem records them), and `st_dev` /
 * `st_ino`, which fstat zeroes: the record carries no file identity yet, and
 * 0 says so. Grows toward a full struct stat as file I/O matures. */
struct stat {
    unsigned long st_dev;
    unsigned long st_ino;
    long st_size;
    unsigned st_mode;
    unsigned st_uid;
    unsigned st_gid;
};

/* The POSIX file-type bits of st_mode. */
#define S_IFMT 0170000
#define S_IFDIR 0040000
#define S_IFREG 0100000
#define S_IFCHR 0020000 /* fstat of a console fd (file.c) */
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
#define S_ISCHR(m) (((m) & S_IFMT) == S_IFCHR)

int fstat(int fd, struct stat *st);

#endif
