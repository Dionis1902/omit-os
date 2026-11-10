/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/spinlock.h>
 *
 * A real test-and-set lock. Omit OS runs on one CPU, but threads are
 * preempted, so two threads can contend: the loser spins until the holder
 * is scheduled again and releases the lock. As on Linux, these plain
 * variants do not disable interrupts and must not be taken from interrupt
 * handlers.
 */
#ifndef _OMIT_LINUXKPI_SPINLOCK_H
#define _OMIT_LINUXKPI_SPINLOCK_H

#include <linux/types.h>

typedef struct {
	volatile unsigned char locked;
} spinlock_t;

static inline void spin_lock_init(spinlock_t *lock)
{
	lock->locked = 0;
}

/* Returns 1 if the lock was taken, 0 if it is held by someone else. */
static inline int spin_trylock(spinlock_t *lock)
{
	return !__atomic_test_and_set(&lock->locked, __ATOMIC_ACQUIRE);
}

static inline void spin_lock(spinlock_t *lock)
{
	while (!spin_trylock(lock))
		__builtin_ia32_pause();
}

static inline void spin_unlock(spinlock_t *lock)
{
	__atomic_clear(&lock->locked, __ATOMIC_RELEASE);
}

#endif
