# ADR 0003: Toolchain, dependency policy and test harness

- Status: accepted
- Date: 2026-11-09

## Decision

### Stable Rust, pinned

The whole tree builds on **stable Rust**, pinned in `rust-toolchain.toml`.
No nightly feature, no `-Zbuild-std`, no custom target JSON: the kernel uses
the built-in `x86_64-unknown-none` target (soft-float, no red zone) and the
loader `x86_64-unknown-uefi`.

Things commonly done with nightly features are done on stable instead:

| Need | Nightly approach | What we do |
| --- | --- | --- |
| Interrupt entry | `extern "x86-interrupt"` | `global_asm!` stubs calling a normal `extern "C"` function with a trap frame |
| Kernel code model / no PIC | custom target JSON | `-C code-model=kernel -C relocation-model=static` in `.cargo/config.toml` |
| Linker script | custom target JSON | `kernel/build.rs` |

Nightly may be adopted later for a specific feature if there is no reasonable
stable alternative; that would get its own ADR.

### Dependencies

| Component | Third-party crates |
| --- | --- |
| Kernel | none |
| `libs/bootproto`, `libs/elf` | none |
| Loader | `uefi` (firmware bindings) and its dependencies |
| `xtask` (host tool only) | `fatfs` (writes the FAT32 system partition) |

A dependency needs a concrete justification: it must replace code that is
both non-trivial and not specific to this OS. Licences are tracked in
[`docs/THIRD_PARTY.md`](../THIRD_PARTY.md).

### `unsafe` policy

- `unsafe_op_in_unsafe_fn` is denied workspace-wide.
- Clippy's `undocumented_unsafe_blocks` and `missing_safety_doc` are denied,
  so CI fails on any `unsafe` block without a `// SAFETY:` justification or
  any `unsafe fn` without a `# Safety` section.
- `libs/elf` is `#![forbid(unsafe_code)]`.

### Build driver: `cargo xtask`

Building the image and running QEMU is done by a small host-side Rust program
(`tools/xtask`) rather than shell scripts or Make. It needs nothing beyond
Cargo and QEMU, runs identically locally and in CI, and builds the GPT/FAT32
disk image itself — with fixed identifiers and no timestamps, so the image is
reproducible.

### Integration test protocol

- QEMU is started with `-device isa-debug-exit,iobase=0xf4,iosize=0x04`.
- The test image's command line contains `qemu-exit`. Only then does the
  kernel write its final status to port `0xF4`: `0x10` after successful
  initialisation (QEMU exits with 33), `0x11` after an initialisation failure,
  panic or fatal exception (35). The flag is never implied, so the port is
  untouched on real hardware.
- Log lines have the fixed shape `[LEVEL] subsystem: message`. A test passes
  only if the exit status **and** the expected serial lines match, within a
  timeout.
- `fault=panic|pagefault|stackoverflow` deliberately crashes the kernel after
  initialisation so the panic handler, the page-fault path and the
  double-fault IST stack are regression-tested rather than assumed to work.

## Consequences

- The tested kernel binary is the shipped kernel binary; test behaviour is
  selected at boot, not at compile time.
- The fault-injection hook is a small amount of test-only code in the kernel.
  It is reachable only via the command line, which requires write access to
  the boot volume — the same access that could replace the kernel outright.
- Release images are byte-for-byte reproducible for a given toolchain and
  source checkout path (verified by rebuilding from a clean `target/`). The
  kernel's debug line tables embed absolute source paths, so builds from
  different directories differ; path remapping is future work.
