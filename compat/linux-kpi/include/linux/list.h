/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS LinuxKPI: <linux/list.h> — circular doubly linked lists.
 *
 * A list is a `struct list_head` that links to itself when empty; members
 * embed a `struct list_head` and are found from it with container_of().
 * Written for Omit OS to the interface Linux drivers use.
 */
#ifndef _OMIT_LINUXKPI_LIST_H
#define _OMIT_LINUXKPI_LIST_H

#include <linux/kernel.h>

struct list_head {
	struct list_head *next, *prev;
};

#define LIST_HEAD_INIT(name) { &(name), &(name) }
#define LIST_HEAD(name) struct list_head name = LIST_HEAD_INIT(name)

static inline void INIT_LIST_HEAD(struct list_head *list)
{
	list->next = list;
	list->prev = list;
}

static inline bool list_empty(const struct list_head *head)
{
	return head->next == head;
}

/* Inserts `entry` right after `head`. */
static inline void list_add(struct list_head *entry, struct list_head *head)
{
	entry->next = head->next;
	entry->prev = head;
	head->next->prev = entry;
	head->next = entry;
}

/* Inserts `entry` right before `head`, i.e. at the end of the list. */
static inline void list_add_tail(struct list_head *entry, struct list_head *head)
{
	list_add(entry, head->prev);
}

/* Removes `entry` from its list. Its own links are cleared so that using
 * it afterwards faults instead of corrupting the list. */
static inline void list_del(struct list_head *entry)
{
	entry->prev->next = entry->next;
	entry->next->prev = entry->prev;
	entry->next = NULL;
	entry->prev = NULL;
}

#define list_entry(ptr, type, member) container_of(ptr, type, member)
#define list_first_entry(head, type, member) list_entry((head)->next, type, member)
#define list_next_entry(pos, member) \
	list_entry((pos)->member.next, __typeof__(*(pos)), member)

#define list_for_each_entry(pos, head, member) \
	for (pos = list_first_entry(head, __typeof__(*pos), member); \
	     &pos->member != (head); \
	     pos = list_next_entry(pos, member))

/* Like list_for_each_entry(), but `pos` may be removed inside the loop. */
#define list_for_each_entry_safe(pos, n, head, member) \
	for (pos = list_first_entry(head, __typeof__(*pos), member), \
	     n = list_next_entry(pos, member); \
	     &pos->member != (head); \
	     pos = n, n = list_next_entry(n, member))

#endif
