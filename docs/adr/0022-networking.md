# ADR 0022: Networking — a first network card, the base of IPv4, UDP sockets

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
- **UDP sockets for programs**, as four system calls: `udp_bind` (a
  descriptor for a local port, chosen by the kernel if asked),
  `udp_send`, `udp_receive` (with a timeout) and `net_info` (the
  interface's addresses, including the name server). Sockets work with
  `poll` and are closed like any descriptor. Each keeps up to 32
  received datagrams; more are dropped, as UDP allows. Sending waits up
  to a second for the next hop to be resolved. A receiver looks for
  frames itself for the first 20 ms of a wait — answers to something
  just sent arrive without waiting for the next timer tick — and then
  sleeps until the receive thread brings one.
- **Protocols above sockets belong to programs, and to a library with
  the SDK's licence**: `libs/inet` (MIT OR Apache-2.0) has the formats
  of DNS questions and answers (RFC 1035) and of TFTP (RFC 1350),
  written from the RFCs and host-tested. The kernel-side library is
  GPL-2.0-only and programs do not link it; the two share no code.
- Programs `host NAME [SERVER[:PORT]]` (looks a name up, by default at
  the name server DHCP announced) and `tftp SERVER FILE [DESTINATION]`
  (fetches a file).

## What works

- 52 host tests in `libs/net`: every format against hand-checked bytes
  and against damage and truncation; the state machine through address
  resolution, routing through the router, answering ARP and echo
  requests, the full DHCP exchange, its retries, refusal and giving up,
  and UDP to and from local ports. 15 host tests in `libs/inet`.
- QEMU test `network`: a VirtIO card on QEMU's user-mode network gets
  `10.0.2.15/24` with router and name server from QEMU's DHCP server;
  `ping 10.0.2.2` resolves the gateway by ARP and gets three replies;
  an address nobody has is reported unreachable; the VirtIO disk next
  to the card still works. Then, from programs: `tftp` fetches four
  files from QEMU's TFTP server — a short one, one of exactly two
  blocks, a 300 KB program compared byte for byte with the system's
  own copy, and a program that is then run; a missing file and an
  absent server are reported. `host` asks a name server that the test
  runs on the host: a known name, an unknown one, a port where nobody
  answers. `nettest` checks the system calls themselves: port
  conflicts, timeouts, `poll`, queueing in order, datagrams cut to the
  buffer, and bad descriptors, sizes and pointers.

## Limitations

- **No TCP**, so nothing that needs a connection: no web, no remote
  shell. This is the largest gap.
- Sockets are IPv4 UDP only, without options: no broadcast permission
  to ask for, no multicast, no connected sockets, no way to learn which
  port the kernel chose. Any program may bind any free port. Nothing is
  sent back when a datagram arrives for a port nobody listens on.
- Name lookup is a program, not a service: `ping` and `tftp` take
  addresses only. `host` asks one server for one IPv4 address, does not
  cache, and reports answers too long for UDP instead of retrying over
  TCP.
- TFTP is read-only and without options (512-byte blocks).
- A program cannot send to this machine's own address.
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
  guest no requests. `host` has not been run against a real name
  server from here: the build environment has no outside network. Real network cards (Intel, Realtek, wireless) have
  no driver.
