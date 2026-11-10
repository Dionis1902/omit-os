// SPDX-License-Identifier: GPL-2.0-only
//! Linker settings for the Windows test programs: console programs whose
//! entry point is `mainCRTStartup`, as for programs built without the C
//! runtime.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bins=/ENTRY:mainCRTStartup");
        println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");
    }
}
