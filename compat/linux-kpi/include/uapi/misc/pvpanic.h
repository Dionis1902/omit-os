/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <uapi/misc/pvpanic.h>
 *
 * Event bits of the pvpanic device, as its specification defines them
 * (QEMU, docs/specs/pvpanic.rst): the guest writes them to the device's
 * register, and reads the set the device supports from the same register.
 */
#ifndef _OMIT_LINUXKPI_UAPI_PVPANIC_H
#define _OMIT_LINUXKPI_UAPI_PVPANIC_H

#include <linux/const.h>

/* The guest has panicked. */
#define PVPANIC_PANICKED _BITUL(0)
/* The guest has panicked and will handle it itself (crash dump kernel). */
#define PVPANIC_CRASH_LOADED _BITUL(1)
/* The guest is shutting down. */
#define PVPANIC_SHUTDOWN _BITUL(2)

#endif
