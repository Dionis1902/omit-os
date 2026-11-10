// SPDX-License-Identifier: GPL-2.0-only
//! Kernel context switching.
//!
//! A thread that is not running is represented by a saved stack pointer.
//! The top of its stack holds the callee-saved registers and a return
//! address; everything else is already saved by the normal call sequence
//! (for a voluntary switch) or by the trap entry code (for a preemption).

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.context, "ax", @progbits

// switch_context(save_rsp: *mut u64 [rdi], load_rsp: u64 [rsi])
.global switch_context
switch_context:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret

// First code a new thread runs: `switch_context` "returns" here with the
// entry function in r12 and its argument in r13 (see `initial_stack`).
.global thread_trampoline
thread_trampoline:
    mov rdi, r12
    mov rsi, r13
    call {thread_start}
    ud2
.text
"#,
    thread_start = sym crate::sched::thread_start,
);

unsafe extern "C" {
    /// Saves the current context, storing its stack pointer in `*save_rsp`,
    /// and resumes the context whose stack pointer is `load_rsp`. Returns
    /// when something switches back to the saved context.
    ///
    /// # Safety
    ///
    /// * `save_rsp` must be valid for a write.
    /// * `load_rsp` must have been produced by an earlier `switch_context`
    ///   or by [`initial_stack`], for a stack that is still mapped and not
    ///   running on any CPU.
    /// * Interrupts must be disabled.
    pub fn switch_context(save_rsp: *mut u64, load_rsp: u64);

    fn thread_trampoline();
}

/// Number of words `switch_context` pops: six registers and the return
/// address.
const FRAME_WORDS: u64 = 7;

/// Prepares a fresh stack so that switching to the returned stack pointer
/// calls `crate::sched::thread_start(entry, argument)`.
///
/// # Safety
///
/// `stack_top` must be the 16-byte-aligned exclusive end of a mapped,
/// writable stack of at least `FRAME_WORDS * 8` bytes that nothing else
/// uses.
pub unsafe fn initial_stack(stack_top: u64, entry: usize, argument: usize) -> u64 {
    let rsp = stack_top - FRAME_WORDS * 8;
    let frame = rsp as *mut [u64; FRAME_WORDS as usize];
    // SAFETY: per the contract the frame lies inside the new stack, which
    // is writable and unused. The layout matches the pops in
    // `switch_context`: r15, r14, r13, r12, rbx, rbp, return address. After
    // the `ret`, RSP equals `stack_top`, so the `call` in the trampoline
    // leaves the stack aligned as the ABI requires.
    unsafe {
        frame.write([
            0,
            0,
            argument as u64,
            entry as u64,
            0,
            0,
            thread_trampoline as usize as u64,
        ]);
    }
    rsp
}
