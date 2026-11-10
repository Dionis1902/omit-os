/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/io.h> — memory-mapped I/O accessors. */
#ifndef _OMIT_LINUXKPI_IO_H
#define _OMIT_LINUXKPI_IO_H

#include <linux/types.h>

static inline u8 readb(const volatile void __iomem *addr)
{
	return *(const volatile u8 *)addr;
}

static inline u16 readw(const volatile void __iomem *addr)
{
	return *(const volatile u16 *)addr;
}

static inline u32 readl(const volatile void __iomem *addr)
{
	return *(const volatile u32 *)addr;
}

static inline void writeb(u8 value, volatile void __iomem *addr)
{
	*(volatile u8 *)addr = value;
}

static inline void writew(u16 value, volatile void __iomem *addr)
{
	*(volatile u16 *)addr = value;
}

static inline void writel(u32 value, volatile void __iomem *addr)
{
	*(volatile u32 *)addr = value;
}

/* The ioread/iowrite family on memory-mapped registers. */
static inline u8 ioread8(const void __iomem *addr)
{
	return readb(addr);
}

static inline void iowrite8(u8 value, void __iomem *addr)
{
	writeb(value, addr);
}

void iounmap(volatile void __iomem *addr);

#endif
