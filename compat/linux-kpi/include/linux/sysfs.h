/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/sysfs.h> — device attributes.
 *
 * Omit OS has no sysfs filesystem. The attributes a driver declares for
 * its devices are available through the kernel (the shell's `devattr`
 * command): reading one calls its show() function, writing calls store().
 */
#ifndef _OMIT_LINUXKPI_SYSFS_H
#define _OMIT_LINUXKPI_SYSFS_H

#include <linux/types.h>

/* Size of the buffer handed to show() functions. */
#define PAGE_SIZE 4096UL

/* Layout shared with the Rust side. */
struct attribute {
	const char *name;
	unsigned short mode;
};

/* Layout shared with the Rust side. */
struct attribute_group {
	const char *name;
	struct attribute **attrs;
};

/*
 * Formats into a show() buffer, which is PAGE_SIZE bytes; returns the
 * number of characters written (src/printk.c).
 */
int sysfs_emit(char *buf, const char *fmt, ...) __attribute__((format(printf, 2, 3)));

#endif
