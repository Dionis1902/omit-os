// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Native device drivers.

pub mod ahci;
pub mod block;
pub mod e1000;
pub mod nvme;
pub mod registry;
pub mod virtio;
pub mod virtio_blk;
pub mod virtio_net;
pub mod xhci;
