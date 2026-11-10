/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/device.h> */
#ifndef _OMIT_LINUXKPI_DEVICE_H
#define _OMIT_LINUXKPI_DEVICE_H

#include <linux/kernel.h>
#include <linux/sysfs.h>

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

/* The same; that the memory is zeroed is more than the caller may rely on. */
static inline void *devm_kmalloc(struct device *dev, size_t size, gfp_t gfp)
{
	return devm_kzalloc(dev, size, gfp);
}

/*
 * Arranges for action(data) to be called when the driver is unbound from
 * the device (most recently added first). If that cannot be arranged, the
 * action is called at once and an error returned.
 */
int devm_add_action_or_reset(struct device *dev, void (*action)(void *), void *data);

/* A device attribute; see <linux/sysfs.h>. Layout shared with the Rust
 * side. */
struct device_attribute {
	struct attribute attr;
	ssize_t (*show)(struct device *dev, struct device_attribute *attr, char *buf);
	ssize_t (*store)(struct device *dev, struct device_attribute *attr,
			 const char *buf, size_t count);
};

#define DEVICE_ATTR_RO(_name) \
	struct device_attribute dev_attr_##_name = { \
		.attr = { .name = #_name, .mode = 0444 }, \
		.show = _name##_show, \
	}
#define DEVICE_ATTR_RW(_name) \
	struct device_attribute dev_attr_##_name = { \
		.attr = { .name = #_name, .mode = 0644 }, \
		.show = _name##_show, \
		.store = _name##_store, \
	}

#endif
