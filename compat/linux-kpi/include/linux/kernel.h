/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/kernel.h> */
#ifndef _OMIT_LINUXKPI_KERNEL_H
#define _OMIT_LINUXKPI_KERNEL_H

#include <linux/types.h>

#define offsetof(type, member) __builtin_offsetof(type, member)

#define container_of(ptr, type, member) \
	((type *)((char *)(ptr) - offsetof(type, member)))

#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define BIT(n) (1UL << (n))

/* Error numbers, with the values Linux uses on x86. */
#define EIO 5
#define ENOMEM 12
#define EBUSY 16
#define ENODEV 19
#define EINVAL 22
#define ERANGE 34

/* Atomicity is irrelevant for the drivers supported so far: they only test
 * flag bits that are set once during probing. */
static inline int test_bit(int nr, const unsigned long *addr)
{
	return (addr[nr / (8 * sizeof(long))] >> (nr % (8 * sizeof(long)))) & 1;
}

static inline void set_bit(int nr, unsigned long *addr)
{
	addr[nr / (8 * sizeof(long))] |= 1UL << (nr % (8 * sizeof(long)));
}

/* Log levels understood by omit_kpi_log(). */
#define OMIT_KPI_LOG_ERR 0
#define OMIT_KPI_LOG_WARN 1
#define OMIT_KPI_LOG_INFO 2

/* Formats a message and hands it to the kernel log (src/printk.c). */
void omit_kpi_printf(int level, const char *prefix, const char *fmt, ...)
	__attribute__((format(printf, 3, 4)));

#define pr_err(fmt, ...) omit_kpi_printf(OMIT_KPI_LOG_ERR, NULL, fmt, ##__VA_ARGS__)
#define pr_warn(fmt, ...) omit_kpi_printf(OMIT_KPI_LOG_WARN, NULL, fmt, ##__VA_ARGS__)
#define pr_info(fmt, ...) omit_kpi_printf(OMIT_KPI_LOG_INFO, NULL, fmt, ##__VA_ARGS__)

#endif
