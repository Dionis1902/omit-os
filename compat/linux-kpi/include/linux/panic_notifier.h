/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/panic_notifier.h> */
#ifndef _OMIT_LINUXKPI_PANIC_NOTIFIER_H
#define _OMIT_LINUXKPI_PANIC_NOTIFIER_H

#include <linux/notifier.h>

/*
 * Called by the Omit OS panic handler, after it has printed its report
 * and before it stops the machine. Callbacks run with interrupts disabled
 * and must not block.
 */
extern struct atomic_notifier_head panic_notifier_list;

#endif
