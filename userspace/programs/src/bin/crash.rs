// SPDX-License-Identifier: GPL-2.0-only
//! Deliberately misbehaves, to show that the kernel contains the damage.
//! Each stage must be stopped by the kernel; the last one ends the process.

#![no_std]
#![no_main]

use omit_rt::abi::Errno;
use omit_rt::println;

omit_rt::entry!(main);

fn main() -> i32 {
    // 1. Hand the kernel pointers it must refuse to follow.
    let kernel_address = 0xFFFF_FFFF_8000_0000u64;
    // SAFETY: this deliberately passes an invalid pointer to `write`; the
    // kernel validates it and returns an error instead of reading it.
    let result = unsafe { omit_rt::syscall(omit_rt::abi::nr::WRITE, 1, kernel_address, 16) };
    println!("write(kernel address) -> {:?}", Errno::from_result(result));
    // SAFETY: as above, with an unmapped user address.
    let result = unsafe { omit_rt::syscall(omit_rt::abi::nr::WRITE, 1, 0x1000, 16) };
    println!(
        "write(unmapped address) -> {:?}",
        Errno::from_result(result)
    );
    // SAFETY: an unknown system call number takes no meaningful arguments.
    let result = unsafe { omit_rt::syscall(9999, 0, 0, 0) };
    println!("syscall 9999 -> {:?}", Errno::from_result(result));

    // 2. Read kernel memory directly. This faults, and the kernel
    //    terminates the process.
    println!("now reading kernel memory directly...");
    // SAFETY: none — this is an intentional invalid access. The CPU raises
    // a page fault before any value is produced, and the kernel ends the
    // process, so no code ever observes the result.
    let value = unsafe { (kernel_address as *const u64).read_volatile() };
    println!("BUG: read {value:#x} from kernel memory");
    1
}
