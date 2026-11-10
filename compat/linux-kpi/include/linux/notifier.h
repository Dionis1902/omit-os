/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/notifier.h> — notifier chains.
 *
 * A chain is a list of callbacks ordered by descending priority. The
 * structures are shared with the Rust side (kernel/src/linuxkpi/notify.rs),
 * which implements registration and calls the chains.
 */
#ifndef _OMIT_LINUXKPI_NOTIFIER_H
#define _OMIT_LINUXKPI_NOTIFIER_H

#include <linux/types.h>

struct notifier_block;

typedef int (*notifier_fn_t)(struct notifier_block *nb, unsigned long action, void *data);

/* Layout shared with the Rust side. */
struct notifier_block {
	notifier_fn_t notifier_call;
	struct notifier_block *next;
	int priority;
};

/* Layout shared with the Rust side. */
struct atomic_notifier_head {
	struct notifier_block *head;
};

#define NOTIFY_DONE 0x0000
#define NOTIFY_OK 0x0001
#define NOTIFY_STOP_MASK 0x8000
#define NOTIFY_BAD (NOTIFY_STOP_MASK | 0x0002)

int atomic_notifier_chain_register(struct atomic_notifier_head *chain,
				   struct notifier_block *nb);
int atomic_notifier_chain_unregister(struct atomic_notifier_head *chain,
				     struct notifier_block *nb);

#endif
