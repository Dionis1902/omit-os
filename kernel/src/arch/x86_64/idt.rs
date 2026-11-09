// SPDX-License-Identifier: GPL-2.0-only
//! Interrupt Descriptor Table, trap entry and CPU exception handling.
//!
//! All 256 vectors are routed through small assembly stubs into
//! [`trap_dispatch`]. Vectors 0-31 are the architectural exceptions and are
//! handled here; vectors 32-255 are hardware interrupts and are forwarded to
//! [`super::interrupts`].
//!
//! Every exception except #BP is fatal for now: the kernel prints the saved
//! machine state and halts.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::gdt::{DOUBLE_FAULT_IST, DescriptorTablePointer, KERNEL_CS};
use super::{cpu, interrupts, qemu};
use crate::kprintln;

const EXCEPTION_COUNT: usize = 32;
const VECTOR_COUNT: usize = 256;
const VECTOR_BREAKPOINT: u64 = 3;
const VECTOR_DOUBLE_FAULT: usize = 8;
const VECTOR_PAGE_FAULT: u64 = 14;

/// Present, DPL 0, 64-bit interrupt gate (interrupts stay disabled on entry).
const GATE_INTERRUPT: u8 = 0x8E;

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        type_attr: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn interrupt_gate(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CS,
            ist,
            type_attr: GATE_INTERRUPT,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

// Read by the CPU on every interrupt for as long as the kernel runs. Written
// exactly once by `init` before being loaded.
static mut IDT: Idt = Idt([IdtEntry::MISSING; 256]);
static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Machine state saved by the exception entry stubs, in stack order.
#[repr(C)]
#[derive(Debug)]
pub struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    /// Exception vector number, pushed by the stub.
    vector: u64,
    /// Error code pushed by the CPU, or 0 for vectors that have none.
    error_code: u64,
    // Pushed by the CPU:
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

// Entry stubs. Each normalises the stack to a `TrapFrame` (pushing a dummy
// error code where the CPU pushes none) and jumps to the common path, which
// saves the general-purpose registers and calls `trap_dispatch` with a
// pointer to the frame.
//
// Stack alignment: in 64-bit mode the CPU aligns RSP to 16 bytes before
// pushing the 5-word interrupt frame. Together with the error code, the
// vector and 15 registers that is 22 words (176 bytes), so RSP is 16-byte
// aligned at the `call`, as the SysV ABI requires. The kernel is built
// without SSE, so there is no vector state to save.
global_asm!(
    r#"
.macro EXC_NOERR n
    .balign 16
exc_stub_\n:
    push 0
    push \n
    jmp exc_common
.endm

.macro EXC_ERR n
    .balign 16
exc_stub_\n:
    push \n
    jmp exc_common
.endm

.section .text.exceptions, "ax", @progbits
EXC_NOERR 0
EXC_NOERR 1
EXC_NOERR 2
EXC_NOERR 3
EXC_NOERR 4
EXC_NOERR 5
EXC_NOERR 6
EXC_NOERR 7
EXC_ERR   8
EXC_NOERR 9
EXC_ERR   10
EXC_ERR   11
EXC_ERR   12
EXC_ERR   13
EXC_ERR   14
EXC_NOERR 15
EXC_NOERR 16
EXC_ERR   17
EXC_NOERR 18
EXC_NOERR 19
EXC_NOERR 20
EXC_ERR   21
EXC_NOERR 22
EXC_NOERR 23
EXC_NOERR 24
EXC_NOERR 25
EXC_NOERR 26
EXC_NOERR 27
EXC_NOERR 28
EXC_ERR   29
EXC_ERR   30
EXC_NOERR 31

// Hardware interrupt vectors never carry an error code.
.altmacro
.macro IRQ_STUB n
    EXC_NOERR \n
.endm
.set vector, 32
.rept 224
    IRQ_STUB %vector
    .set vector, vector + 1
.endr

exc_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    cld
    mov rdi, rsp
    call {dispatch}
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq

.section .rodata.exceptions, "a", @progbits
.balign 8
.global trap_stub_table
trap_stub_table:
.macro STUB_ADDRESS n
    .quad exc_stub_\n
.endm
.set vector, 0
.rept 256
    STUB_ADDRESS %vector
    .set vector, vector + 1
.endr
.noaltmacro
.text
"#,
    dispatch = sym trap_dispatch,
);

unsafe extern "C" {
    /// Addresses of the 256 trap entry stubs, defined in the assembly above.
    static trap_stub_table: [u64; VECTOR_COUNT];
}

const EXCEPTION_NAMES: [&str; EXCEPTION_COUNT] = [
    "#DE divide error",
    "#DB debug",
    "NMI",
    "#BP breakpoint",
    "#OF overflow",
    "#BR bound range exceeded",
    "#UD invalid opcode",
    "#NM device not available",
    "#DF double fault",
    "coprocessor segment overrun",
    "#TS invalid TSS",
    "#NP segment not present",
    "#SS stack-segment fault",
    "#GP general protection fault",
    "#PF page fault",
    "reserved (15)",
    "#MF x87 floating-point error",
    "#AC alignment check",
    "#MC machine check",
    "#XM SIMD floating-point error",
    "#VE virtualization exception",
    "#CP control protection exception",
    "reserved (22)",
    "reserved (23)",
    "reserved (24)",
    "reserved (25)",
    "reserved (26)",
    "reserved (27)",
    "#HV hypervisor injection exception",
    "#VC VMM communication exception",
    "#SX security exception",
    "reserved (31)",
];

/// Number of #BP exceptions handled; used by [`self_test`].
static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);

/// Builds and loads the IDT.
///
/// Must be called after [`super::gdt::init`], because the gates refer to the
/// kernel code segment and the double-fault IST slot set up there.
///
/// # Panics
///
/// Panics if called more than once.
pub fn init() {
    assert!(
        !INITIALISED.swap(true, Ordering::AcqRel),
        "idt::init called twice"
    );

    let idt = &raw mut IDT;
    // SAFETY: the `INITIALISED` guard makes this the only access to `IDT`
    // from Rust, before the table is visible to the CPU. `trap_stub_table` is
    // an immutable array of 256 addresses emitted by the `global_asm!` block
    // above.
    unsafe {
        for (vector, &stub) in trap_stub_table.iter().enumerate() {
            let ist = if vector == VECTOR_DOUBLE_FAULT {
                DOUBLE_FAULT_IST
            } else {
                0
            };
            (*idt).0[vector] = IdtEntry::interrupt_gate(stub, ist);
        }
    }

    let pointer = DescriptorTablePointer {
        limit: (size_of::<Idt>() - 1) as u16,
        base: idt as u64,
    };
    // SAFETY: `pointer` describes the static, fully initialised IDT whose
    // present gates all point at valid entry stubs in the kernel text.
    unsafe {
        asm!("lidt [{}]", in(reg) &raw const pointer,
             options(readonly, nostack, preserves_flags));
    }
}

/// Raises a breakpoint exception and checks that it was delivered to the
/// kernel's handler and that execution resumed afterwards.
///
/// This exercises the whole exception path: IDT lookup, the entry stub,
/// register save/restore and `iretq`.
pub fn self_test() -> bool {
    let before = BREAKPOINTS.load(Ordering::Relaxed);
    // SAFETY: `int3` raises #BP, which `trap_dispatch` handles by
    // counting and returning; the stub restores every register and `iretq`
    // restores the flags.
    unsafe { asm!("int3", options(nomem, nostack, preserves_flags)) };
    BREAKPOINTS.load(Ordering::Relaxed) == before + 1
}

/// Rust entry point for all CPU exceptions; called from `exc_common`.
extern "C" fn trap_dispatch(frame: &mut TrapFrame) {
    if frame.vector >= EXCEPTION_COUNT as u64 {
        interrupts::dispatch(frame.vector);
        return;
    }
    if frame.vector == VECTOR_BREAKPOINT {
        BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let name = EXCEPTION_NAMES
        .get(frame.vector as usize)
        .copied()
        .unwrap_or("unknown");
    kprintln!();
    kprintln!("==== KERNEL EXCEPTION ====");
    kprintln!(
        "exception: {} (vector {}, error code {:#x})",
        name,
        frame.vector,
        frame.error_code
    );
    if frame.vector == VECTOR_PAGE_FAULT {
        let e = frame.error_code;
        kprintln!(
            "fault address: {:#018x} ({} {} in {} mode{})",
            cpu::read_cr2(),
            if e & 1 != 0 {
                "protection violation on"
            } else {
                "non-present page on"
            },
            if e & 16 != 0 {
                "instruction fetch"
            } else if e & 2 != 0 {
                "write"
            } else {
                "read"
            },
            if e & 4 != 0 { "user" } else { "kernel" },
            if e & 8 != 0 { ", reserved bit set" } else { "" },
        );
    }
    kprintln!(
        "rip={:#018x} cs={:#06x} rflags={:#010x}",
        frame.rip,
        frame.cs,
        frame.rflags
    );
    kprintln!("rsp={:#018x} ss={:#06x}", frame.rsp, frame.ss);
    kprintln!(
        "rax={:#018x} rbx={:#018x} rcx={:#018x}",
        frame.rax,
        frame.rbx,
        frame.rcx
    );
    kprintln!(
        "rdx={:#018x} rsi={:#018x} rdi={:#018x}",
        frame.rdx,
        frame.rsi,
        frame.rdi
    );
    kprintln!(
        "rbp={:#018x} r8 ={:#018x} r9 ={:#018x}",
        frame.rbp,
        frame.r8,
        frame.r9
    );
    kprintln!(
        "r10={:#018x} r11={:#018x} r12={:#018x}",
        frame.r10,
        frame.r11,
        frame.r12
    );
    kprintln!(
        "r13={:#018x} r14={:#018x} r15={:#018x}",
        frame.r13,
        frame.r14,
        frame.r15
    );
    kprintln!("backtrace:");
    kprintln!("  #0 {:#018x}", frame.rip);
    let mut depth = 1;
    super::backtrace(frame.rbp, |address| {
        kprintln!("  #{depth} {address:#018x}");
        depth += 1;
    });
    kprintln!("==========================");

    qemu::exit(qemu::ExitCode::Failure);
    super::halt_forever();
}
