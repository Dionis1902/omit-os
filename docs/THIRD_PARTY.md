# Third-party code

Omit OS's own code is under several licences, by component; see
[`COPYING`](../COPYING). The repository contains no Windows, Wine or Proton
code, and no Linux code other than the individual drivers listed below.

This file lists every third-party crate in `Cargo.lock`, where it ends up, and
its licence. Regenerate the list with `cargo metadata` when dependencies
change.

## Linked into the boot loader (`omit-boot`)

| Crate | Licence | Notes |
| --- | --- | --- |
| `uefi`, `uefi-raw`, `uefi-macros` | MIT OR Apache-2.0 | UEFI bindings; used under MIT |
| `uguid` | MIT OR Apache-2.0 | used under MIT |
| `ucs2` | MPL-2.0 | UCS-2 string handling for UEFI; MPL-2.0 permits combination with GPL-2.0 (§3.3, "Secondary Licenses") |
| `bit_field` | Apache-2.0 OR MIT | used under MIT |
| `bitflags` 2.x | MIT OR Apache-2.0 | used under MIT |
| `cfg-if` | MIT OR Apache-2.0 | used under MIT |
| `log` | MIT OR Apache-2.0 | used under MIT |
| `ptr_meta` | MIT | |

Build-time only (procedural macros, not linked into any shipped binary):
`ptr_meta_derive` (MIT), `proc-macro2`, `quote`, `syn`
(MIT OR Apache-2.0), `unicode-ident` ((MIT OR Apache-2.0) AND Unicode-3.0).

## Linked into the kernel (`omit-kernel`)

| Crate | Licence | Notes |
| --- | --- | --- |
| `noto-sans-mono-bitmap` | MIT (crate); SIL Open Font License 1.1 (the embedded Noto Sans Mono glyph data) | Font for the framebuffer console. The OFL permits bundling and embedding the font in software under any licence; the font data itself stays under the OFL. |

## Linux kernel code linked into the kernel

Individual Linux device drivers are compiled, unmodified, against the Omit
OS LinuxKPI layer. The authoritative list, with upstream paths, versions and
checksums, is [`drivers/linux/README.md`](../drivers/linux/README.md).

| File | Licence | Copyright |
| --- | --- | --- |
| `drivers/linux/watchdog/i6300esb.c` | GPL-2.0-or-later | (c) 2004 Google Inc.; (c) 2005 David Härdeman; see the file header |
| `drivers/linux/misc/pvpanic/pvpanic.c`, `pvpanic-pci.c`, `pvpanic.h` | GPL-2.0-or-later | (c) 2013 Fujitsu; (c) 2018 ZTE; (c) 2021 Oracle; see the file headers |

The LinuxKPI headers in `compat/linux-kpi/` and the runtime in the kernel are
original Omit OS code under GPL-2.0-only. They declare interfaces compatible with Linux's but
contain no Linux source.

## Linked into programs that make encrypted connections (`fetch`, through `omit-tls`)

None of this is linked into the kernel or the boot loader. The crates
arrive through three direct dependencies: `rustls` (the TLS protocol),
`rustls-rustcrypto` (the cryptography: the RustCrypto project's
pure-Rust implementations; an early, **unaudited** release, pinned to an
exact version) and `webpki-roots`.

| Crates | Licence |
| --- | --- |
| `rustls` | Apache-2.0 OR ISC OR MIT |
| `rustls-webpki` (two versions), `untrusted` | ISC |
| `rustls-pki-types`, `rustls-rustcrypto` | MIT OR Apache-2.0 |
| `webpki-roots` | CDLA-Permissive-2.0 — the crate is Mozilla's list of certificate authorities as data; see [`ATTRIBUTION.md`](../ATTRIBUTION.md) |
| `aead`, `aes`, `aes-gcm`, `base16ct`, `base64ct`, `block-buffer`, `chacha20`, `chacha20poly1305`, `cipher`, `const-oid`, `cpufeatures`, `crypto-bigint`, `crypto-common`, `ctr`, `der`, `digest`, `ecdsa`, `ed25519`, `elliptic-curve`, `ff`, `ghash`, `group`, `hkdf`, `hmac`, `inout`, `opaque-debug`, `p256`, `p384`, `pem-rfc7468`, `pkcs1`, `pkcs5`, `pkcs8`, `poly1305`, `polyval`, `primeorder`, `rfc6979`, `rsa`, `sec1`, `sha2`, `signature`, `spki`, `universal-hash`, `zeroize` | MIT OR Apache-2.0 (RustCrypto) |
| `curve25519-dalek`, `ed25519-dalek`, `x25519-dalek`, `subtle` | BSD-3-Clause |
| `cfg-if`, `getrandom`, `lazy_static`, `num-bigint-dig`, `num-integer`, `num-iter`, `num-traits`, `once_cell`, `paste`, `ppv-lite86`, `rand`, `rand_chacha`, `rand_core`, `smallvec`, `typenum` | MIT OR Apache-2.0 |
| `zerocopy` | BSD-2-Clause OR Apache-2.0 OR MIT |
| `generic-array`, `libm`, `spin` | MIT |

`ring` appears in `Cargo.lock` as an optional dependency of one of
these and is **not built** for any target (`cargo tree -i ring` finds no
path to it).

## Host tooling only (`xtask`; never part of the OS image)

The `network` test's web server speaks TLS with the same `rustls` and
`rustls-rustcrypto` as above (and their dependencies, plus `libc`,
`zeroize_derive` and `curve25519-dalek-derive`, MIT OR Apache-2.0).

| Crate | Licence |
| --- | --- |
| `fatfs` | MIT |
| `bitflags` 1.x | MIT OR Apache-2.0 |
| `byteorder` | Unlicense OR MIT |
| `log` | MIT OR Apache-2.0 |

## Not redistributed

QEMU and the OVMF/EDK II firmware are used for testing but are neither
included in this repository nor in the generated disk image.
