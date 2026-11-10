// SPDX-License-Identifier: GPL-2.0-only
//! Kernel build script: passes the linker script to the linker and compiles
//! the C parts of the kernel (the LinuxKPI runtime and the Linux drivers
//! built on it).

use std::path::{Path, PathBuf};
use std::process::Command;

/// C sources, relative to the workspace root, with the module name passed
/// to each as `KBUILD_MODNAME`.
const C_SOURCES: &[(&str, &str)] = &[
    ("compat/linux-kpi/src/printk.c", "omit_kpi_printk"),
    ("compat/linux-kpi/src/layout.c", "omit_kpi_layout"),
    ("drivers/linux/watchdog/i6300esb.c", "i6300esb"),
];

/// Flags for freestanding kernel code: no host headers or libraries, no red
/// zone (interrupts use the same stack), no floating point or vector
/// registers (the kernel does not save them), addresses in the top 2 GiB.
const C_FLAGS: &[&str] = &[
    "-std=gnu11",
    "-ffreestanding",
    "-nostdinc",
    "-fno-stack-protector",
    "-fno-pic",
    "-fno-pie",
    "-mno-red-zone",
    "-mcmodel=kernel",
    "-mno-sse",
    "-mno-mmx",
    "-mno-80387",
    "-msoft-float",
    "-fno-asynchronous-unwind-tables",
    "-fno-omit-frame-pointer",
    "-O2",
    "-Wall",
    "-Wextra",
    "-Wno-unused-parameter",
    "-Werror",
];

/// Picks the C compiler: `$OMIT_CC` if set, otherwise clang, otherwise gcc.
fn compiler() -> (String, bool) {
    if let Ok(cc) = std::env::var("OMIT_CC") {
        let is_clang = cc.contains("clang");
        return (cc, is_clang);
    }
    for (candidate, is_clang) in [("clang", true), ("gcc", false)] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return (candidate.into(), is_clang);
        }
    }
    panic!(
        "no C compiler found: building the kernel's Linux drivers needs clang or gcc \
         (set OMIT_CC to use a specific one)"
    );
}

fn compile(root: &Path, out_dir: &Path, source: &str, module: &str) -> PathBuf {
    let (cc, is_clang) = compiler();
    let object = out_dir.join(format!("{module}.o"));
    let mut command = Command::new(&cc);
    if is_clang {
        // clang is a cross compiler; gcc on x86-64 already targets it.
        command.arg("--target=x86_64-unknown-none-elf");
    }
    let status = command
        .args(C_FLAGS)
        .arg(format!(
            "-I{}",
            root.join("compat/linux-kpi/include").display()
        ))
        .arg(format!("-DKBUILD_MODNAME={module}"))
        .arg("-c")
        .arg(root.join(source))
        .arg("-o")
        .arg(&object)
        .status()
        .unwrap_or_else(|error| panic!("cannot run {cc}: {error}"));
    assert!(status.success(), "{cc} failed to compile {source}");
    object
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo"));
    let root = dir.parent().expect("kernel/ is inside the workspace");

    println!("cargo:rerun-if-changed=linker.ld");
    println!(
        "cargo:rustc-link-arg-bins=--script={}/linker.ld",
        dir.display()
    );
    // The image is loaded by our own loader, which honours segment
    // permissions; there is no reason to pad segments to 2 MiB.
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");

    println!("cargo:rerun-if-env-changed=OMIT_CC");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("compat/linux-kpi/include").display()
    );
    for (source, module) in C_SOURCES {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
        let object = compile(root, &out_dir, source, module);
        println!("cargo:rustc-link-arg-bins={}", object.display());
    }
}
