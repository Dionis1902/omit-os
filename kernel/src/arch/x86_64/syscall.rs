// SPDX-License-Identifier: GPL-2.0-only
//! System call entry via `syscall`/`sysret`.
//!
//! `syscall` is the fast path into the kernel: unlike an interrupt gate it
//! does not touch the stack or the TSS, so the entry code must switch to
//! the thread's kernel stack by hand before running any Rust.
//!
//! The kernel currently runs on one CPU, so the two per-CPU values the
//! entry code needs (the current thread's kernel stack and a scratch slot
//! for the user stack pointer) are plain statics. With SMP they move into
//! a per-CPU area addressed through `GS` and `swapgs`.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu;
use super::gdt::{KERNEL_CS, KERNEL_DS};

const MSR_EFER: u32 = 0xC000_0080;
const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;
const EFER_SYSCALL_ENABLE: u64 = 1 << 0;

/// RFLAGS bits cleared on entry: trap, interrupt, direction, and alignment
/// check (which doubles as the SMAP override).
const ENTRY_FLAG_MASK: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 18);

/// Top of the current thread's kernel stack; loaded into RSP on entry.
#[unsafe(no_mangle)]
static SYSCALL_KERNEL_RSP: AtomicU64 = AtomicU64::new(0);
/// Holds the user RSP between entry and the switch to the kernel stack.
/// Only touched with interrupts disabled.
#[unsafe(no_mangle)]
static SYSCALL_USER_RSP: AtomicU64 = AtomicU64::new(0);

// On entry: RCX = user RIP, R11 = user RFLAGS, RSP = user stack,
// RAX = system call number, arguments in RDI, RSI, RDX, R10, R8.
// Interrupts are off (SFMASK).
global_asm!(
    r#"
.section .text.syscall, "ax", @progbits
.global syscall_entry
syscall_entry:
    mov [rip + SYSCALL_USER_RSP], rsp
    mov rsp, [rip + SYSCALL_KERNEL_RSP]

    // Everything needed to return, plus the registers the ABI promises to
    // preserve. Ten words keep RSP 16-byte aligned for the call.
    push qword ptr [rip + SYSCALL_USER_RSP]
    push r11
    push rcx
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8

    // dispatch(number, a0, a1, a2, a3, a4) in the SysV calling convention.
    mov r9, r8
    mov r8, r10
    mov rcx, rdx
    mov rdx, rsi
    mov rsi, rdi
    mov rdi, rax
    // The user stack pointer is saved on this thread's own kernel stack
    // now, so being preempted or blocking is safe from here on.
    sti
    call {dispatch}
    cli

    add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    pop rcx
    pop r11
    // Do not leak kernel values through the registers the ABI does not
    // preserve.
    xor edi, edi
    xor esi, esi
    xor edx, edx
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    pop rsp
    sysretq
.text
"#,
    dispatch = sym crate::syscall::dispatch,
);

unsafe extern "C" {
    fn syscall_entry();
}

/// Enables the `syscall` instruction and points it at the kernel's entry
/// code. Must run after the GDT is loaded.
pub fn init() {
    // `syscall` loads CS from STAR[47:32] and SS from that + 8; `sysret`
    // (64-bit) loads SS from STAR[63:48] + 8 and CS from STAR[63:48] + 16.
    //
    // The value for `sysret` carries privilege level 3 in its low bits.
    // Intel CPUs force those bits to 3 in both selectors, but AMD CPUs
    // only do so for CS and load SS exactly as computed. With level 0
    // here, user code on AMD would run with SS = 0x18 instead of 0x1B,
    // which goes unnoticed until an interrupt arrives in user mode: the
    // CPU saves that SS, and returning with `iretq` then faults because
    // its privilege level does not match CS. (Found by the KVM tests on
    // an AMD host; software emulation behaves like Intel.)
    let sysret_base = KERNEL_DS | 3;
    let star = (u64::from(sysret_base) << 48) | (u64::from(KERNEL_CS) << 32);
    // SAFETY: these MSRs exist on every x86-64 CPU. The entry point is the
    // kernel's `syscall_entry`; the selectors match the GDT (checked by the
    // constant assertions in `gdt`); the flag mask guarantees interrupts
    // are off on entry, which the entry code relies on.
    unsafe {
        cpu::write_msr(MSR_STAR, star);
        cpu::write_msr(MSR_LSTAR, syscall_entry as *const () as u64);
        cpu::write_msr(MSR_SFMASK, ENTRY_FLAG_MASK);
        cpu::write_msr(MSR_EFER, cpu::read_msr(MSR_EFER) | EFER_SYSCALL_ENABLE);
    }
}

/// Sets the kernel stack that system calls made by the next thread to run
/// will use.
///
/// # Safety
///
/// Same contract as [`super::gdt::set_kernel_stack`].
pub unsafe fn set_kernel_stack(stack_top: u64) {
    SYSCALL_KERNEL_RSP.store(stack_top, Ordering::Relaxed);
}

global_asm!(
    r#"
.section .text.enter_user, "ax", @progbits
// enter_user(rip [rdi], rsp [rsi], user_cs [rdx], user_ss [rcx],
//            argument [r8]) -> !
// The argument arrives in the user code's RDI; every other general
// register is zero.
.global enter_user
enter_user:
    cli
    push rcx            // SS
    push rsi            // RSP
    push 0x202          // RFLAGS: interrupts enabled
    push rdx            // CS
    push rdi            // RIP
    xor eax, eax
    xor ebx, ebx
    xor ecx, ecx
    xor edx, edx
    xor esi, esi
    mov rdi, r8
    xor ebp, ebp
    xor r8d, r8d
    xor r9d, r9d
    xor r10d, r10d
    xor r11d, r11d
    xor r12d, r12d
    xor r13d, r13d
    xor r14d, r14d
    xor r15d, r15d
    iretq
.text
"#
);

unsafe extern "C" {
    /// Drops to user mode at `rip` with stack pointer `rsp`, all other
    /// registers zeroed and interrupts enabled. Never returns; the thread
    /// re-enters the kernel only through interrupts and system calls, on
    /// the kernel stack registered with [`set_kernel_stack`].
    ///
    /// # Safety
    ///
    /// The active address space must map `rip` as user-executable code and
    /// `rsp` as the top of a user-writable stack, and the calling thread's
    /// kernel stack must be registered as described above.
    pub fn enter_user(rip: u64, rsp: u64, user_cs: u64, user_ss: u64, argument: u64) -> !;
}
