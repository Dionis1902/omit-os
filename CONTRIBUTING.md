# Contributing

## Sign your commits (DCO)

Every commit must carry a sign-off line naming its author:

```text
Signed-off-by: Your Name <you@example.org>
```

`git commit -s` adds it. By signing off you certify the
[Developer Certificate of Origin 1.1](https://developercertificate.org/):
in short, that you wrote the change or otherwise have the right to
submit it under the licence of the files it touches. CI rejects commits
without a sign-off that matches the commit's author.

## Keep the code clean

The full rules are in [`IP_POLICY.md`](IP_POLICY.md). In short:

- Windows compatibility is written only from public documentation and
  the observed behaviour of test programs — never from leaked,
  shared-source or decompiled Microsoft code, and not by copying Wine or
  ReactOS.
- Every source file of the Windows compatibility code starts with a
  `Sources:` comment, and its behaviour is pinned by tests that run
  real programs.
- Imported Linux code stays unmodified in `drivers/linux/`, with its
  upstream notices, and is listed in [`PROVENANCE.md`](PROVENANCE.md).
- No large blocks recalled from other codebases; commit messages say
  what the human maintainer decided or changed.
- Assets are original or openly licensed; nothing is extracted from
  other systems, and no proprietary fonts are bundled.

## One repository, several licences

`COPYING` says which licence applies to which directory. When you add or
change files:

- **A new file takes the licence of the component it is in** and starts
  with two lines (in the comment syntax of its language):

  <!-- REUSE-IgnoreStart -->
  ```text
  // SPDX-FileCopyrightText: 2026 Omit OS contributors
  // SPDX-License-Identifier: <the component's licence>
  ```
  <!-- REUSE-IgnoreEnd -->

  Files that cannot hold a comment (images, lock files) are listed in
  `REUSE.toml` instead.
- **A new crate** states its licence explicitly in `Cargo.toml`
  (`license = "..."`), has `publish = false` unless it is part of the
  SDK, and is added to the table in `COPYING`, to
  `tools/check-licenses.py` and to `tools/check-deps.sh`.
- **Imported Linux driver code** (`drivers/linux/`) is kept exactly as
  upstream has it, notices included.
- **Do not change the licence of existing files.**

### What may be linked into what

| Component | May depend on |
| --- | --- |
| Kernel, boot loader, kernel-side libraries (`GPL-2.0-only`) | `GPL-2.0-only`, `GPL-2.0-or-later`, MIT, BSD, ISC, Zlib, Unicode-3.0, and anything offered as `MIT OR Apache-2.0` (used under MIT). **Never** GPL-3.0, LGPL-3.0, AGPL, or Apache-2.0-only code. |
| User-space system components (`GPL-3.0-or-later`) | the above except `GPL-2.0-only` code, plus Apache-2.0, MPL-2.0 and `GPL-3.0-or-later` |
| SDK crates (`MIT OR Apache-2.0`) | permissive licences only — nothing GPL, LGPL or AGPL anywhere below them, except `libs/abi`, whose licence carries the system call exception |

### No copying across components

Kernel and user space meet only at the system call interface and the
IPC the kernel provides; they share no source files. Do not copy code
between components with incompatible licences — not from the kernel
into the SDK or the desktop, not from the desktop into the kernel or the
SDK. If two components need the same code, it belongs in a library whose
licence both can use (see `libs/mm`), and that is a decision to discuss
first.

## Checks to run before you push

```bash
cargo fmt --all --check
```

```bash
cargo clippy --all-targets --locked -- -D warnings
```

```bash
cargo test --locked
```

```bash
cargo xtask test
```

Licensing (CI runs all of these):

```bash
reuse lint
```

```bash
python3 tools/check-licenses.py
```

```bash
sh tools/check-deps.sh
```

```bash
python3 tools/check-ip.py
```

`reuse` is installed with `pip install reuse`, cargo-deny with
`cargo install cargo-deny --locked`. `reuse lint` checks that every file
has licensing information and every licence its text;
`check-licenses.py` that each source file carries the licence of its
component; `check-deps.sh` runs `cargo deny check licenses` on every
crate with the policy of its component (`deny.toml` for the kernel side,
`tools/deny-userspace.toml`, `tools/deny-sdk.toml`).

## Everything else

The README describes how the project works: small verifiable steps,
tests for what is claimed, decisions recorded in `docs/adr/`, and plain
statements of what does not work.
