# ADR 0004: Physical memory management

- Status: accepted
- Date: 2026-11-09

## Context

The kernel needs to hand out physical page frames: single frames for page
tables and, soon, for the heap and user memory; contiguous runs for huge
pages and DMA buffers. The project's stated goals are low, predictable
allocation latency and low fragmentation, and its rules ask for safe,
host-testable code where possible.

| Option | Alloc/free cost | Contiguous blocks | Fragmentation control | Metadata |
| --- | --- | --- | --- | --- |
| Bitmap, first/next fit | O(n) scan | yes, by scanning | none | 1 bit/frame |
| Free-list stack | O(1) | no | none | none (intrusive) |
| **Binary buddy** | O(log max-order), bounded | yes, power-of-two, aligned | merges on free | per-frame record |

## Decision

A **binary buddy allocator** with orders 0-10 (4 KiB to 4 MiB), implemented in
`libs/mm` as safe Rust over a caller-provided metadata slice.

- **Index-linked, non-intrusive free lists.** Each frame has a 12-byte
  `FrameMeta` (two `u32` links, order, state). Free lists are linked through
  frame indices in that array instead of through pointers stored inside the
  free pages. Compared with the classic intrusive design this costs about
  0.3 % of managed memory, and buys three things: the allocator contains no
  `unsafe` and runs unmodified in host unit tests; it never touches free
  memory, so a stray write to a free page cannot corrupt it; and invalid frees
  (double free, wrong order, interior pointer) are detected from the metadata
  and rejected.
- **The kernel side is thin.** `kernel/src/mm/frame.rs` picks the memory to
  manage (usable RAM above 1 MiB), places the metadata array at the start of
  the largest usable region, and wraps the allocator in an interrupt-safe
  spinlock. `free` is an `unsafe fn`: the allocator can detect a malformed
  free, but not that the caller still uses the memory.
- **Maximum order 10.** Enough for 2 MiB huge pages (order 9) with headroom.
  1 GiB allocations are out of scope; if they are ever needed they will come
  from boot-time reservation, not from the general allocator.
- **Self-test at boot.** The integration with real memory cannot be tested on
  the host, so the kernel allocates, pattern-tests and frees blocks of several
  orders during initialisation and refuses to continue if anything is off.

Device memory is mapped through `map_mmio` into a dedicated virtual range
(`0xFFFF_C000_0000_0000`, 64 GiB) as writable, non-executable, cache-disabled
pages with a guard page after each mapping. It still edits the page tables
inherited from the loader.

## Consequences

- One global lock. Fine for one CPU; SMP will need per-CPU frame caches in
  front of the buddy allocator to keep the common path lock-free. That is the
  usual design and does not change the allocator underneath.
- No NUMA awareness and no physical-address constraints (for example
  "below 4 GiB" for legacy DMA) yet. Both fit as multiple allocator instances
  ("zones") later.
- Metadata is sized for the range from frame 0 to the top of usable RAM, so
  large holes in the physical address space waste a little metadata.
- Boot-reclaimable memory is not handed to the allocator yet. Some firmware
  keeps using boot-services memory after `ExitBootServices`, so reclaiming it
  safely needs care and testing on real machines.
- Still to do in this milestone: a kernel heap, and kernel-owned page tables
  with unmapping, TLB invalidation and correct cache attributes for the
  direct map.
