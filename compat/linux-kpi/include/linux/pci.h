/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/pci.h> */
#ifndef _OMIT_LINUXKPI_PCI_H
#define _OMIT_LINUXKPI_PCI_H

#include <linux/device.h>
#include <linux/io.h>

#define PCI_ANY_ID (~0U)
#define PCI_VENDOR_ID_INTEL 0x8086
#define PCI_VENDOR_ID_REDHAT 0x1b36
#define PCI_DEVICE_ID_INTEL_ESB_9 0x25ab

/* Layout shared with the Rust side; see <linux/device.h>. */
struct pci_device_id {
	u32 vendor, device;
	u32 subvendor, subdevice;
	u32 class, class_mask;
	unsigned long driver_data;
};

#define PCI_DEVICE(vend, dev) \
	.vendor = (vend), .device = (dev), \
	.subvendor = PCI_ANY_ID, .subdevice = PCI_ANY_ID

/* Layout shared with the Rust side. */
struct pci_dev {
	struct device dev;
	u16 vendor;
	u16 device;
	u8 bus_number;
	u8 devfn;
};

/* Layout shared with the Rust side. */
struct pci_driver {
	const char *name;
	const struct pci_device_id *id_table;
	int (*probe)(struct pci_dev *dev, const struct pci_device_id *id);
	void (*remove)(struct pci_dev *dev);
	/* Attributes of every device the driver is bound to (NULL-terminated
	 * list of groups), or NULL. */
	const struct attribute_group **dev_groups;
};

int pci_register_driver(struct pci_driver *driver);

int pci_enable_device(struct pci_dev *dev);
void pci_disable_device(struct pci_dev *dev);
int pci_request_region(struct pci_dev *dev, int bar, const char *name);
void pci_release_region(struct pci_dev *dev, int bar);
void __iomem *pci_ioremap_bar(struct pci_dev *dev, int bar);

/*
 * "Managed" variants: what they set up is undone automatically when the
 * driver is unbound. (Device mappings are never removed in Omit OS, and
 * enabling a device has nothing to undo, so these are the plain functions.)
 */
static inline int pcim_enable_device(struct pci_dev *dev)
{
	return pci_enable_device(dev);
}

/* `maxlen` 0 means the whole region. */
static inline void __iomem *pcim_iomap(struct pci_dev *dev, int bar, unsigned long maxlen)
{
	(void)maxlen;
	return pci_ioremap_bar(dev, bar);
}

int pci_read_config_byte(const struct pci_dev *dev, int where, u8 *value);
int pci_read_config_word(const struct pci_dev *dev, int where, u16 *value);
int pci_read_config_dword(const struct pci_dev *dev, int where, u32 *value);
int pci_write_config_byte(const struct pci_dev *dev, int where, u8 value);
int pci_write_config_word(const struct pci_dev *dev, int where, u16 value);
int pci_write_config_dword(const struct pci_dev *dev, int where, u32 value);

/*
 * On Linux this generates the module's init and exit functions. Here it
 * generates one function that the kernel calls to register the driver; its
 * name is derived from KBUILD_MODNAME, which the build passes per file.
 */
#define __omit_paste(a, b) a##b
#define __omit_init_name(mod) __omit_paste(omit_linux_driver_init_, mod)
#define module_pci_driver(drv) \
	int __omit_init_name(KBUILD_MODNAME)(void) \
	{ \
		return pci_register_driver(&(drv)); \
	}

#endif
