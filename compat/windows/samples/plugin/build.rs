// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Linker settings for a Windows library built without the C runtime: its
//! entry point is `_DllMainCRTStartup`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-cdylib-link-arg=/ENTRY:_DllMainCRTStartup");
    }
}
