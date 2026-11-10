// SPDX-License-Identifier: GPL-2.0-only
/*
 * Omit OS LinuxKPI: structure layout cross-check.
 *
 * Several structures are shared between the C headers and their Rust
 * mirrors in kernel/src/linuxkpi. The Rust side compares its own sizes and
 * offsets with the values exported here at start-up and refuses to run
 * Linux drivers if they differ.
 */

#include <linux/notifier.h>
#include <linux/pci.h>
#include <linux/reboot.h>
#include <linux/watchdog.h>

const unsigned long omit_kpi_layout[] = {
	sizeof(struct device),
	sizeof(struct pci_dev),
	offsetof(struct pci_dev, vendor),
	offsetof(struct pci_dev, devfn),
	sizeof(struct pci_device_id),
	sizeof(struct pci_driver),
	offsetof(struct pci_driver, remove),
	sizeof(struct watchdog_info),
	sizeof(struct watchdog_ops),
	offsetof(struct watchdog_ops, set_timeout),
	sizeof(struct watchdog_device),
	offsetof(struct watchdog_device, timeout),
	offsetof(struct watchdog_device, status),
	offsetof(struct pci_driver, dev_groups),
	sizeof(struct notifier_block),
	offsetof(struct notifier_block, priority),
	sizeof(struct attribute),
	sizeof(struct attribute_group),
	sizeof(struct device_attribute),
	offsetof(struct device_attribute, store),
	sizeof(struct sys_off_data),
};
const unsigned long omit_kpi_layout_len = ARRAY_SIZE(omit_kpi_layout);
