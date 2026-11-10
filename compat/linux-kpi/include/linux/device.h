/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/device.h> */
#ifndef _OMIT_LINUXKPI_DEVICE_H
#define _OMIT_LINUXKPI_DEVICE_H

#include <linux/kernel.h>

/*
 * The layout of this structure is shared with the Rust side
 * (kernel/src/linuxkpi); keep the two in sync. A size check at start-up
 * catches mismatches.
 */
struct device {
	void *driver_data;
	/* "bus:device.function", NUL-terminated; used as the log prefix. */
	char name[16];
};

static inline void dev_set_drvdata(struct device *dev, void *data)
{
	dev->driver_data = data;
}

static inline void *dev_get_drvdata(const struct device *dev)
{
	return dev->driver_data;
}

#define dev_err(dev, fmt, ...) \
	omit_kpi_printf(OMIT_KPI_LOG_ERR, (dev)->name, fmt, ##__VA_ARGS__)
#define dev_warn(dev, fmt, ...) \
	omit_kpi_printf(OMIT_KPI_LOG_WARN, (dev)->name, fmt, ##__VA_ARGS__)
#define dev_info(dev, fmt, ...) \
	omit_kpi_printf(OMIT_KPI_LOG_INFO, (dev)->name, fmt, ##__VA_ARGS__)

#define GFP_KERNEL 0

/* Zeroed memory that is released automatically when the driver is unbound
 * from the device. */
void *devm_kzalloc(struct device *dev, size_t size, gfp_t gfp);

#endif
