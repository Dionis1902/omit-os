// SPDX-License-Identifier: GPL-2.0-only
//! Exercises the program heap: small and large allocations, growth,
//! release back to the kernel, and reuse.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_rt::println;

omit_rt::entry!(main);

fn main() -> i32 {
    // Grows through every size class into multi-megabyte mappings.
    let mut numbers: Vec<u64> = Vec::new();
    for value in 0..1_000_000u64 {
        numbers.push(value);
    }
    let sum: u64 = numbers.iter().sum();
    if sum != 999_999 * 1_000_000 / 2 {
        println!("memtest: FAILED: vector sum is {sum}");
        return 1;
    }
    println!("memtest: vector of {} numbers ok", numbers.len());
    drop(numbers);

    // Many small objects alive at once, in a structure that allocates per
    // node.
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    for key in 0..5000u32 {
        map.insert(key, format!("value-{key}"));
    }
    if map.len() != 5000 || map.get(&4321).map(String::as_str) != Some("value-4321") {
        println!("memtest: FAILED: map contents are wrong");
        return 1;
    }
    println!("memtest: map of {} strings ok", map.len());
    drop(map);

    // A single large block: mapped directly, written end to end, returned
    // to the kernel, and requested again.
    for round in 0..3 {
        let mut block: Box<[u8]> = alloc::vec![0u8; 8 << 20].into_boxed_slice();
        let last = block.len() - 1;
        block[0] = 0x5A;
        block[last] = 0xA5;
        if block[0] != 0x5A || block[last] != 0xA5 || block[last / 2] != 0 {
            println!("memtest: FAILED: large block round {round}");
            return 1;
        }
    }
    println!("memtest: 8 MiB block allocated and released 3 times ok");
    0
}
