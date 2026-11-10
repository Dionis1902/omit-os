// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Sources: an original test program; Microsoft Learn reference pages of the
//   functions it calls. It is part of the behaviour tests of the Windows
//   compatibility layer (QEMU tests windows-programs and desktop).
//! Linker settings for the Windows test programs: programs whose
//! entry point is `mainCRTStartup`, as for programs built without the C
//! runtime.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bins=/ENTRY:mainCRTStartup");
        println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");
        // The graphical test program is marked as one (the later option
        // wins). It keeps the same entry point: without the C runtime
        // there is no `WinMain` wrapper.
        println!("cargo:rustc-link-arg-bin=wingui=/SUBSYSTEM:WINDOWS");
    }
}
