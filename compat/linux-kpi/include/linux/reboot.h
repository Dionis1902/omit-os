/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/reboot.h> — callbacks for powering off.
 *
 * Handlers registered for SYS_OFF_MODE_POWER_OFF are called, in descending
 * priority order, when Omit OS shuts down in an orderly way (the shell
 * exits). Other modes are accepted and recorded but there is nothing in
 * Omit OS yet that would trigger them.
 */
#ifndef _OMIT_LINUXKPI_REBOOT_H
#define _OMIT_LINUXKPI_REBOOT_H

#include <linux/err.h>
#include <linux/notifier.h>

enum sys_off_mode {
	SYS_OFF_MODE_POWER_OFF_PREPARE,
	SYS_OFF_MODE_POWER_OFF,
	SYS_OFF_MODE_RESTART_PREPARE,
	SYS_OFF_MODE_RESTART,
};

#define SYS_OFF_PRIO_LOW (-128)
#define SYS_OFF_PRIO_DEFAULT 0
#define SYS_OFF_PRIO_HIGH 192

/* Layout shared with the Rust side. */
struct sys_off_data {
	int mode;
	void *cb_data;
	const char *cmd;
	struct device *dev;
};

struct sys_off_handler;

/* Returns the handler, or an ERR_PTR(). */
struct sys_off_handler *register_sys_off_handler(enum sys_off_mode mode, int priority,
						 int (*callback)(struct sys_off_data *data),
						 void *cb_data);
/* Does nothing for NULL or an ERR_PTR(), as on Linux. */
void unregister_sys_off_handler(struct sys_off_handler *handler);

#endif
