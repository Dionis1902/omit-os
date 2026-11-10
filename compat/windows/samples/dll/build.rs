// SPDX-License-Identifier: GPL-2.0-only
//! Linker settings for a Windows library built without the C runtime: its
//! entry point is `_DllMainCRTStartup`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-cdylib-link-arg=/ENTRY:_DllMainCRTStartup");
    }
}
