// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Passes the user program linker script to the linker.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=user.ld");
    println!("cargo:rustc-link-arg-bins=--script={dir}/user.ld");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
}
