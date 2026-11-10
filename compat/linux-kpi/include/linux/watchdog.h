/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/watchdog.h> */
#ifndef _OMIT_LINUXKPI_WATCHDOG_H
#define _OMIT_LINUXKPI_WATCHDOG_H

#include <linux/device.h>

#define WDIOF_CARDRESET 0x0020
#define WDIOF_SETTIMEOUT 0x0080
#define WDIOF_MAGICCLOSE 0x0100
#define WDIOF_KEEPALIVEPING 0x8000

#define WATCHDOG_NOWAYOUT 0

/* Bits of watchdog_device.status. */
#define WDOG_ACTIVE 0
#define WDOG_NO_WAY_OUT 1
#define WDOG_STOP_ON_REBOOT 2
#define WDOG_STOP_ON_UNREGISTER 4

struct watchdog_device;

/* Layout shared with the Rust side; see <linux/device.h>. */
struct watchdog_info {
	u32 options;
	u32 firmware_version;
	u8 identity[32];
};

/* Layout shared with the Rust side. */
struct watchdog_ops {
	struct module *owner;
	int (*start)(struct watchdog_device *);
	int (*stop)(struct watchdog_device *);
	int (*ping)(struct watchdog_device *);
	unsigned int (*status)(struct watchdog_device *);
	int (*set_timeout)(struct watchdog_device *, unsigned int);
	unsigned int (*get_timeleft)(struct watchdog_device *);
};

/* Layout shared with the Rust side. */
struct watchdog_device {
	const struct watchdog_info *info;
	const struct watchdog_ops *ops;
	unsigned int bootstatus;
	unsigned int timeout;
	unsigned int min_timeout;
	unsigned int max_timeout;
	void *driver_data;
	unsigned long status;
};

static inline void watchdog_set_nowayout(struct watchdog_device *wdd, bool nowayout)
{
	if (nowayout)
		set_bit(WDOG_NO_WAY_OUT, &wdd->status);
}

static inline void watchdog_stop_on_reboot(struct watchdog_device *wdd)
{
	set_bit(WDOG_STOP_ON_REBOOT, &wdd->status);
}

static inline void watchdog_stop_on_unregister(struct watchdog_device *wdd)
{
	set_bit(WDOG_STOP_ON_UNREGISTER, &wdd->status);
}

static inline void watchdog_set_drvdata(struct watchdog_device *wdd, void *data)
{
	wdd->driver_data = data;
}

static inline void *watchdog_get_drvdata(struct watchdog_device *wdd)
{
	return wdd->driver_data;
}

int watchdog_init_timeout(struct watchdog_device *wdd, unsigned int timeout_parm,
			  struct device *dev);
int watchdog_register_device(struct watchdog_device *wdd);
void watchdog_unregister_device(struct watchdog_device *wdd);

#endif
