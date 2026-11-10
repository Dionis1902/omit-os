/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/err.h> — error numbers carried in pointers.
 *
 * As on Linux, the last 4095 addresses are never valid objects and encode
 * a negated error number instead.
 */
#ifndef _OMIT_LINUXKPI_ERR_H
#define _OMIT_LINUXKPI_ERR_H

#include <linux/kernel.h>

#define MAX_ERRNO 4095

static inline void *ERR_PTR(long error)
{
	return (void *)error;
}

static inline long PTR_ERR(const void *ptr)
{
	return (long)ptr;
}

static inline bool IS_ERR(const void *ptr)
{
	return (unsigned long)ptr >= (unsigned long)-MAX_ERRNO;
}

#endif
