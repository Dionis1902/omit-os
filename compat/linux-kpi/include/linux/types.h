/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/types.h>
 *
 * Written for Omit OS; declares the subset of the Linux kernel programming
 * interface needed by the drivers in drivers/linux/.
 */
#ifndef _OMIT_LINUXKPI_TYPES_H
#define _OMIT_LINUXKPI_TYPES_H

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef signed char s8;
typedef short s16;
typedef int s32;
typedef long long s64;

typedef u8 __u8;
typedef u16 __u16;
typedef u32 __u32;
typedef u64 __u64;

typedef unsigned long size_t;
typedef long ssize_t;
typedef unsigned long uintptr_t;
typedef u64 phys_addr_t;
typedef u64 resource_size_t;
typedef unsigned int gfp_t;

typedef _Bool bool;
#define true 1
#define false 0

#define NULL ((void *)0)

/* Annotations checked by sparse on Linux; meaningless to the compiler. */
#define __iomem
#define __user
#define __init
#define __exit
#define __must_check

#endif
