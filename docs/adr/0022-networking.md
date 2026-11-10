# ADR 0022: Networking — a first network card and the base of IPv4

- Status: accepted
- Date: 2026-11-10

## Context

The system had no network at all. Networking is a large area; this ADR
covers the first step and says where it stops.

## Decision

- **Protocol code lives in a library, `libs/net`, that does no I/O and
  reads no clock.** Formats (Ethernet II, ARP, IPv4 without options or
  fragments, ICMP echo, UDP, the client's DHCP messages, the Internet
  checksum) are pure functions over byte slices. The behaviour of an
  interface — ARP cache, DHCP client, answering and sending echo
  requests — is one state machine (`stack::Stack`) fed with received
  frames and the current time, sending through a trait. The kernel
  supplies the card, the clock and the threads. This is what allows the
  logic to be tested on the build machine with the test playing the
  rest of the network, including cases an emulator's built-in network
  never produces (a silent or refusing DHCP server, a neighbour that
  changes its hardware address, damaged packets).
- **Written from the specifications**: RFC 826, 791, 792, 768, 1071,
  2131 and 2132, and the VirtIO 1.x specification for the card. No code
  from another network stack was used or consulted.
- **One native driver, VirtIO network** (`kernel/src/drivers/virtio_net.rs`),
  because that is what can be tested here. The parts it shares with the
  VirtIO block driver — finding the register blocks, feature
  negotiation, split virtqueues — moved into
  `kernel/src/drivers/virtio.rs`, and the block driver now uses them.
- **Polling, like every other driver so far.** A kernel thread looks
  for received frames at every timer tick (100 Hz); a thread waiting
  for an answer looks continuously while it waits, which is why `ping`
  reports times far below a tick.
- **One interface, `eth0`,** the first card found, configured by DHCP at
  start-up. Start-up waits up to two seconds for the answer so that the
  first command typed finds the network ready; without an answer DHCP
  gives up after five attempts, in the background.
- Shell commands `ifconfig` (addresses, counters, known neighbours) and
  `ping ADDRESS [COUNT]`.

## What works

- 48 host tests in `libs/net`: every format against hand-checked bytes
  and against damage and truncation; the state machine through address
  resolution, routing through the router, answering ARP and echo
  requests, the full DHCP exchange, its retries, refusal and giving up.
- QEMU test `network`: a VirtIO card on QEMU's user-mode network gets
  `10.0.2.15/24` with router and name server from QEMU's DHCP server;
  `ping 10.0.2.2` resolves the gateway by ARP and gets three replies;
  an address nobody has is reported unreachable; the VirtIO disk next
  to the card still works.

## Limitations

- **Programs cannot use the network**: there are no sockets, no system
  calls, and no TCP. Only the kernel's own `ping` sends anything beyond
  DHCP and ARP. UDP exists only as far as DHCP needs it.
- No DNS: the name server's address is recorded and unused.
- No IPv6. IPv4 packets with options, and fragments, are dropped.
- A DHCP lease is never renewed or released, and the address is kept
  after it expires. There is no way to set an address by hand.
- ARP entries do not expire (sixteen entries; an entry is corrected when
  its owner announces a change, and the oldest is replaced when the
  table is full). No duplicate-address detection.
- The machine cannot ping its own address (nothing is looped back).
- One card only; the link state is not read; no interrupts, no
  offloading, one page per frame, one frame sent at a time.
- **Tested only against QEMU's emulated card and its built-in network.**
  Answering echo requests and replies arriving through a router are
  covered by host tests only, since QEMU's user-mode network sends the
  guest no requests. Real network cards (Intel, Realtek, wireless) have
  no driver.
