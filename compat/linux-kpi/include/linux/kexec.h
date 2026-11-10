/* SPDX-License-Identifier: GPL-2.0-only */
/* Omit OS LinuxKPI: <linux/kexec.h> */
#ifndef _OMIT_LINUXKPI_KEXEC_H
#define _OMIT_LINUXKPI_KEXEC_H

#include <linux/types.h>

/* Omit OS cannot load a crash kernel, so one is never loaded. */
static inline bool kexec_crash_loaded(void)
{
	return false;
}

#endif
