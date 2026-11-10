/* SPDX-FileCopyrightText: 2026 Omit OS contributors */
/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Omit OS kernel image layout.
 *
 * The kernel is a statically linked ELF64 executable in the top 2 GiB of the
 * address space (`-C code-model=kernel`). KERNEL_BASE must match
 * `omit_bootproto::KERNEL_BASE`; the loader refuses images linked below it.
 *
 * Three page-aligned PT_LOAD segments give the loader what it needs to
 * enforce W^X: text (R+X), rodata (R), data+bss (R+W).
 */

ENTRY(_start)

KERNEL_BASE = 0xFFFFFFFF80000000;

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R+X */
    rodata PT_LOAD FLAGS(4); /* R   */
    data   PT_LOAD FLAGS(6); /* R+W */
}

SECTIONS
{
    . = KERNEL_BASE;
    __kernel_start = .;

    .text : ALIGN(4K) {
        __text_start = .;
        *(.text .text.*)
        __text_end = .;
    } :text

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4K) {
        *(.data .data.*)
        *(.got .got.*)
    } :data

    .bss : ALIGN(4K) {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
