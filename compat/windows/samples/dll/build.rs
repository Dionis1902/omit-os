// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
// Sources: an original test program; Microsoft Learn reference pages of the
//   functions it calls. It is part of the behaviour tests of the Windows
//   compatibility layer (QEMU tests windows-programs and desktop).
//! Linker settings for a Windows library built without the C runtime: its
//! entry point is `_DllMainCRTStartup`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-cdylib-link-arg=/ENTRY:_DllMainCRTStartup");
    }
}
