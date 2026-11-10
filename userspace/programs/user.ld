/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Layout of an Omit OS user program: a statically linked, fixed-address
 * ELF64 executable starting at 4 MiB, with separate page-aligned segments
 * for code (R+X), constants (R) and data (R+W). The kernel's program loader
 * refuses segments that are both writable and executable.
 */

ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x400000;

    .text : ALIGN(4K) {
        *(.text._start)
        *(.text .text.*)
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

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
