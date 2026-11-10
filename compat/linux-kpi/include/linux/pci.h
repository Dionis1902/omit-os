/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/pci.h> */
#ifndef _OMIT_LINUXKPI_PCI_H
#define _OMIT_LINUXKPI_PCI_H

#include <linux/device.h>
#include <linux/io.h>

#define PCI_ANY_ID (~0U)
#define PCI_VENDOR_ID_INTEL 0x8086
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
};

int pci_register_driver(struct pci_driver *driver);

int pci_enable_device(struct pci_dev *dev);
void pci_disable_device(struct pci_dev *dev);
int pci_request_region(struct pci_dev *dev, int bar, const char *name);
void pci_release_region(struct pci_dev *dev, int bar);
void __iomem *pci_ioremap_bar(struct pci_dev *dev, int bar);

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
