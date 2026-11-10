/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/limits.h> */
#ifndef _OMIT_LINUXKPI_LIMITS_H
#define _OMIT_LINUXKPI_LIMITS_H

#define INT_MAX ((int)(~0U >> 1))
#define INT_MIN (-INT_MAX - 1)
#define UINT_MAX (~0U)

#endif
