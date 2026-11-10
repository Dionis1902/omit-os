/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/kstrtox.h> — strict string-to-number parsing. */
#ifndef _OMIT_LINUXKPI_KSTRTOX_H
#define _OMIT_LINUXKPI_KSTRTOX_H

#include <linux/types.h>

/*
 * Parses all of `s` as an unsigned number in `base` (0 selects the base
 * from a "0x" or "0" prefix; with base 16 a "0x" prefix is accepted). One
 * trailing newline is allowed. Returns 0, -EINVAL for anything that is not
 * a number, or -ERANGE if it does not fit.
 */
int __must_check kstrtouint(const char *s, unsigned int base, unsigned int *res);

#endif
