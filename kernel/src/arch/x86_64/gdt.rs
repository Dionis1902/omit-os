// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Global Descriptor Table and Task State Segment.
//!
//! Long mode ignores segmentation almost entirely, but the CPU still needs a
//! code segment descriptor with the L bit, flat data segments, and a TSS to
//! find the Interrupt Stack Table. The firmware's GDT lives in memory the
//! kernel does not own, so it is replaced before anything else.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// Selector of the kernel code segment.
pub const KERNEL_CS: u16 = 0x08;
/// Selector of the kernel data segment.
pub const KERNEL_DS: u16 = 0x10;
/// Selector of the user data segment (requested privilege level 3).
pub const USER_DS: u16 = 0x18 | 3;
/// Selector of the user code segment (requested privilege level 3).
pub const USER_CS: u16 = 0x20 | 3;
const TSS_SELECTOR: u16 = 0x28;

// `syscall` and `sysret` derive their selectors from fixed offsets, which
// dictates this order: kernel code, kernel data, user data, user code.
const _: () = assert!(KERNEL_DS == KERNEL_CS + 8);
const _: () = assert!(USER_DS == (KERNEL_DS + 8) | 3 && USER_CS == (KERNEL_DS + 16) | 3);
// `sysret` computes the user selectors by plain addition from a base that
// already has privilege level 3 (see `syscall::init`): the sums must be
// exactly the user selectors, without relying on the CPU to fix up the
// low bits.
const _: () = assert!((KERNEL_DS | 3) + 8 == USER_DS && (KERNEL_DS | 3) + 16 == USER_CS);

/// Interrupt Stack Table slot (1-based, as encoded in IDT entries) used by
/// the double-fault handler.
pub const DOUBLE_FAULT_IST: u8 = 1;

/// Size of the dedicated double-fault stack.
pub const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

/// 64-bit Task State Segment (Intel SDM vol. 3, "64-Bit Task-State Segment").
#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    /// Stack pointers loaded on privilege-level changes to rings 0-2.
    rsp: [u64; 3],
    reserved1: u64,
    /// Interrupt Stack Table: known-good stacks for selected vectors.
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    /// Offset of the I/O permission bitmap; `size_of::<Tss>()` means "none".
    iomap_base: u16,
}

const TSS_SIZE: usize = size_of::<Tss>();
const _: () = assert!(TSS_SIZE == 104);

#[repr(C, align(16))]
struct DoubleFaultStack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Null, kernel code, kernel data, user data, user code, and the two-slot
/// 64-bit TSS descriptor.
#[repr(C, align(8))]
struct Gdt([u64; 7]);

/// Operand of `lgdt`/`lidt`.
#[repr(C, packed)]
pub(super) struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

// Present, ring 0, code, execute/read, 64-bit (L=1).
const KERNEL_CODE_DESCRIPTOR: u64 = 0x00AF_9A00_0000_FFFF;
// Present, ring 0, data, read/write.
const KERNEL_DATA_DESCRIPTOR: u64 = 0x00CF_9200_0000_FFFF;
// Present, ring 3, data, read/write.
const USER_DATA_DESCRIPTOR: u64 = 0x00CF_F200_0000_FFFF;
// Present, ring 3, code, execute/read, 64-bit (L=1).
const USER_CODE_DESCRIPTOR: u64 = 0x00AF_FA00_0000_FFFF;

// These tables are read by the CPU for as long as the kernel runs, so they
// need a fixed address and static lifetime. They are written exactly once,
// by `init`, before being handed to the CPU, and never accessed from Rust
// afterwards.
static mut DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; DOUBLE_FAULT_STACK_SIZE]);
static mut TSS: Tss = Tss {
    reserved0: 0,
    rsp: [0; 3],
    reserved1: 0,
    ist: [0; 7],
    reserved2: 0,
    reserved3: 0,
    iomap_base: TSS_SIZE as u16,
};
static mut GDT: Gdt = Gdt([0; 7]);

static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Address range of the double-fault stack, for backtrace validation.
pub fn double_fault_stack_range() -> core::ops::Range<u64> {
    let start = &raw const DOUBLE_FAULT_STACK as u64;
    start..start + DOUBLE_FAULT_STACK_SIZE as u64
}

/// Builds and loads the kernel GDT and TSS and reloads every segment
/// register.
///
/// # Panics
///
/// Panics if called more than once.
pub fn init() {
    assert!(
        !INITIALISED.swap(true, Ordering::AcqRel),
        "gdt::init called twice"
    );

    let tss = &raw mut TSS;
    let gdt = &raw mut GDT;
    let df_stack_top = double_fault_stack_range().end;

    // SAFETY: the `INITIALISED` guard above makes this the only code that
    // ever accesses the three statics, and it runs once on the boot CPU
    // before the tables are visible to the processor, so the writes cannot
    // race with anything. The pointers come from `&raw mut` on live statics
    // and are valid and aligned (`Tss` is packed, alignment 1).
    unsafe {
        (*tss).ist[usize::from(DOUBLE_FAULT_IST) - 1] = df_stack_top;

        let base = tss as u64;
        let limit = (TSS_SIZE - 1) as u64;
        (*gdt).0 = [
            0,
            KERNEL_CODE_DESCRIPTOR,
            KERNEL_DATA_DESCRIPTOR,
            USER_DATA_DESCRIPTOR,
            USER_CODE_DESCRIPTOR,
            // Low half: limit, base[0..32], type 0x9 (available 64-bit TSS),
            // present.
            (limit & 0xFFFF)
                | ((base & 0x00FF_FFFF) << 16)
                | (0x89 << 40)
                | (((limit >> 16) & 0xF) << 48)
                | (((base >> 24) & 0xFF) << 56),
            // High half: base[32..64].
            base >> 32,
        ];
    }

    let pointer = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: gdt as u64,
    };

    // SAFETY: `pointer` describes the fully initialised static GDT above.
    // Selector 0x08 is a valid 64-bit ring-0 code segment and 0x10 a valid
    // data segment, so reloading CS (via a far return to the next
    // instruction) and the data segment registers keeps the CPU in the same
    // mode and privilege level. `TSS_SELECTOR` is a valid available TSS, as
    // `ltr` requires. FS and GS are loaded with the null selector, which is
    // permitted in 64-bit mode. Interrupts are disabled, so nothing can
    // observe the intermediate state.
    unsafe {
        asm!(
            "lgdt [{ptr}]",
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov ss, {ds:x}",
            "mov fs, {null:x}",
            "mov gs, {null:x}",
            "ltr {tss:x}",
            ptr = in(reg) &raw const pointer,
            cs = in(reg) u64::from(KERNEL_CS),
            tmp = out(reg) _,
            ds = in(reg) u64::from(KERNEL_DS),
            null = in(reg) 0u64,
            tss = in(reg) u64::from(TSS_SELECTOR),
            options(preserves_flags),
        );
    }
}

/// Sets the stack the CPU switches to when an interrupt or exception
/// arrives while it is executing user code (`TSS.RSP0`).
///
/// Called by the scheduler with the top of the next thread's kernel stack.
///
/// # Safety
///
/// `stack_top` must be the top of a mapped kernel stack that is in use by
/// nothing but the thread about to run, and interrupts must be disabled.
pub unsafe fn set_kernel_stack(stack_top: u64) {
    let tss = &raw mut TSS;
    // SAFETY: the TSS is only written here and in `init`. Interrupts are
    // disabled (function contract) and there is one CPU, so the CPU cannot
    // read `rsp[0]` during the update. The field is in a packed struct and
    // is written unaligned.
    unsafe { (&raw mut (*tss).rsp[0]).write_unaligned(stack_top) };
}
