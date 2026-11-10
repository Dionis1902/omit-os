/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/module.h>
 *
 * Omit OS links Linux drivers statically into the kernel, so there are no
 * loadable modules: the metadata macros expand to nothing and module
 * parameters keep their compiled-in defaults.
 */
#ifndef _OMIT_LINUXKPI_MODULE_H
#define _OMIT_LINUXKPI_MODULE_H

#include <linux/kernel.h>

struct module;
#define THIS_MODULE ((struct module *)0)

#define __stringify_1(x...) #x
#define __stringify(x...) __stringify_1(x)
#define __MODULE_STRING(x) __stringify(x)

#define module_param(name, type, perm)
#define MODULE_PARM_DESC(parm, desc)
#define MODULE_DEVICE_TABLE(type, name)

#define __OMIT_MODULE_INFO(tag, line) \
	_Static_assert(1, #tag)
#define MODULE_AUTHOR(x) _Static_assert(1, "author")
#define MODULE_DESCRIPTION(x) _Static_assert(1, "description")
#define MODULE_LICENSE(x) _Static_assert(1, "license")

#endif
