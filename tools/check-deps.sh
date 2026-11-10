#!/bin/sh
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Checks the licences of everything each component links, with
# cargo-deny, against the policy for that component (see COPYING and
# CONTRIBUTING.md). Run from the repository root.
set -eu
check() {
    policy="$1"
    shift
    for crate in "$@"; do
        echo "== $crate ($policy)"
        cargo deny --manifest-path "$crate/Cargo.toml" --config "$policy" check licenses
    done
}
# Linked into the kernel or the boot loader: GPL-2.0-compatible only.
check deny.toml kernel boot shell \
    libs/acpi libs/bootproto libs/elf libs/ext2 libs/fat libs/gpt libs/hwdb libs/net libs/rng libs/term libs/vfs \
    libs/abi
# User-space system components and host tools.
check tools/deny-userspace.toml userspace/programs userspace/tls compat/windows/pe compat/windows/win32 \
    compat/windows/samples compat/windows/samples/dll compat/windows/samples/plugin tools/xtask
# The SDK: permissive all the way down (the system call interface aside).
check tools/deny-sdk.toml userspace/rt libs/window libs/gfx libs/inet libs/mm
