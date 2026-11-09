// SPDX-License-Identifier: GPL-2.0-only
//! Passes the kernel linker script to the linker.

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rustc-link-arg-bins=--script={dir}/linker.ld");
    // The image is loaded by our own loader, which honours segment
    // permissions; there is no reason to pad segments to 2 MiB.
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
}
