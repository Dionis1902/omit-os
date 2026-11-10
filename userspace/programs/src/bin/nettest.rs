// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! `nettest` — tests the UDP and TCP socket system calls.
//!
//!     nettest SERVER PORT WEB-PORT
//!
//! Needs a network interface with an address, and a name server at
//! SERVER, port PORT, that answers every question (the system's test
//! harness runs one); it is used as something that sends a datagram back.
//! WEB-PORT is the port of a web server on the same host, and the port
//! after it must be one nothing listens on.

#![no_std]
#![no_main]

use omit_rt::abi::net::{INFO_CONFIGURED, INFO_PRESENT, UDP_PAYLOAD_MAX};
use omit_rt::abi::{Errno, nr};
use omit_rt::net::{TcpListener, TcpStream, UdpSocket};
use omit_rt::println;

omit_rt::entry!(main);

fn interface() -> Result<(), &'static str> {
    let info = omit_rt::net::info();
    if info.flags & INFO_PRESENT == 0 || info.flags & INFO_CONFIGURED == 0 {
        return Err("no configured interface is reported");
    }
    if info.address == [0; 4] || info.mask == [0; 4] || info.mac == [0; 6] {
        return Err("the interface is reported without address, mask or hardware address");
    }
    // SAFETY: deliberately a bad pointer; the kernel must refuse it.
    let bad = unsafe { omit_rt::syscall(nr::NET_INFO, 0x10, 0, 0) };
    if Errno::from_result(bad) != Some(Errno::Fault) {
        return Err("net_info wrote to a bad address");
    }
    Ok(())
}

fn ports() -> Result<(), &'static str> {
    let first = UdpSocket::bind(5000).map_err(|_| "cannot bind port 5000")?;
    if UdpSocket::bind(5000).err() != Some(Errno::Busy) {
        return Err("a port was bound twice");
    }
    if UdpSocket::bind(68).err() != Some(Errno::Busy) {
        return Err("the DHCP client's port was given away");
    }
    // SAFETY: plain integer argument, deliberately out of range.
    let huge = unsafe { omit_rt::syscall(nr::UDP_BIND, 65536, 0, 0) };
    if Errno::from_result(huge) != Some(Errno::InvalidArgument) {
        return Err("a port number above 65535 was accepted");
    }
    drop(first);
    // Closing the socket frees the port.
    drop(UdpSocket::bind(5000).map_err(|_| "a closed socket's port stayed taken")?);
    // Ports chosen by the kernel differ from one another.
    let a = UdpSocket::bind(0).map_err(|_| "cannot bind a free port")?;
    let b = UdpSocket::bind(0).map_err(|_| "cannot bind a second free port")?;
    if a.fd() == b.fd() {
        return Err("two sockets share a descriptor");
    }
    Ok(())
}

fn waiting() -> Result<(), &'static str> {
    let socket = UdpSocket::bind(0).map_err(|_| "cannot bind")?;
    let mut buffer = [0u8; 64];
    if socket.receive_from(&mut buffer, Some(0)).err() != Some(Errno::WouldBlock) {
        return Err("receiving without waiting did not report WouldBlock");
    }
    let before = omit_rt::uptime_ms();
    if socket.receive_from(&mut buffer, Some(150)).err() != Some(Errno::TimedOut) {
        return Err("receiving with a timeout did not report TimedOut");
    }
    let waited = omit_rt::uptime_ms() - before;
    if !(140..1000).contains(&waited) {
        return Err("a 150 ms timeout took far too little or too much time");
    }
    if omit_rt::poll(&[socket.fd()], 50, false) != Ok(0) {
        return Err("poll reports an empty socket as readable");
    }
    Ok(())
}

fn refusals(server: [u8; 4], port: u16) -> Result<(), &'static str> {
    let socket = UdpSocket::bind(0).map_err(|_| "cannot bind")?;
    static BIG: [u8; UDP_PAYLOAD_MAX as usize + 1] = [0; UDP_PAYLOAD_MAX as usize + 1];
    if socket.send_to(&BIG, server, port).err() != Some(Errno::InvalidArgument) {
        return Err("a datagram that does not fit a frame was accepted");
    }
    socket
        .send_to(&BIG[..UDP_PAYLOAD_MAX as usize], server, port)
        .map_err(|_| "the largest datagram was refused")?;
    // A file is not a socket, and neither is a descriptor that is not open.
    let to = omit_rt::abi::net::SocketAddr::default();
    for fd in [omit_rt::abi::STDOUT, 31] {
        // SAFETY: the pointers refer to live values; the descriptor is
        // deliberately wrong.
        let sent = unsafe {
            omit_rt::syscall4(
                nr::UDP_SEND,
                fd,
                &raw const to as u64,
                BIG.as_ptr() as u64,
                1,
            )
        };
        if Errno::from_result(sent) != Some(Errno::BadFileDescriptor) {
            return Err("sending on something that is not a socket was accepted");
        }
    }
    // SAFETY: deliberately bad pointers; the kernel must refuse them.
    let bad_address = unsafe { omit_rt::syscall4(nr::UDP_SEND, socket.fd(), 0x10, 0x10, 1) };
    if Errno::from_result(bad_address) != Some(Errno::Fault) {
        return Err("sending from a bad address was accepted");
    }
    // Reading and writing a socket like a file is refused.
    let mut byte = [0u8; 1];
    if omit_rt::read(socket.fd(), &mut byte).is_ok() || omit_rt::write(socket.fd(), &byte).is_ok() {
        return Err("a socket was read or written like a file");
    }
    Ok(())
}

/// A question for the name server: header and the root name.
fn question(id: u8) -> [u8; 17] {
    [0, id, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1]
}

fn exchange(server: [u8; 4], port: u16) -> Result<(), &'static str> {
    let socket = UdpSocket::bind(0).map_err(|_| "cannot bind")?;
    socket
        .send_to(&question(1), server, port)
        .map_err(|_| "cannot send")?;
    // The answer makes the socket readable.
    if omit_rt::poll(&[socket.fd()], 2000, false) != Ok(1) {
        return Err("poll did not report the answer");
    }
    let mut buffer = [0u8; 64];
    let (count, from, from_port) = socket
        .receive_from(&mut buffer, Some(0))
        .map_err(|_| "the answer poll reported cannot be received")?;
    if from != server || from_port != port {
        return Err("the answer's sender is reported wrongly");
    }
    if count != 17 || buffer[1] != 1 || buffer[2] & 0x80 == 0 {
        return Err("the answer is not the server's answer to the question");
    }
    // A buffer that is too small gets the beginning of the datagram.
    socket
        .send_to(&question(2), server, port)
        .map_err(|_| "cannot send")?;
    let mut small = [0u8; 4];
    match socket.receive_from(&mut small, Some(2000)) {
        Ok((4, _, _)) if small[1] == 2 => {}
        _ => return Err("a short buffer did not get the start of the datagram"),
    }
    // ... and the rest of that datagram is gone, not kept for later.
    if socket.receive_from(&mut buffer, Some(0)).err() != Some(Errno::WouldBlock) {
        return Err("the cut-off part of a datagram was delivered separately");
    }
    // Several answers queue up in order.
    for id in 10..15 {
        socket
            .send_to(&question(id), server, port)
            .map_err(|_| "cannot send")?;
    }
    for id in 10..15 {
        match socket.receive_from(&mut buffer, Some(2000)) {
            Ok((17, _, _)) if buffer[1] == id => {}
            _ => return Err("queued answers were lost or out of order"),
        }
    }
    // A bad address for the sender is refused.
    socket
        .send_to(&question(3), server, port)
        .map_err(|_| "cannot send")?;
    let _ = omit_rt::poll(&[socket.fd()], 2000, false);
    // SAFETY: the buffer is live and writable; the address for the
    // sender is deliberately bad.
    let bad = unsafe {
        omit_rt::syscall5(
            nr::UDP_RECEIVE,
            socket.fd(),
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0x10,
            0,
        )
    };
    if Errno::from_result(bad) != Some(Errno::Fault) {
        return Err("the sender was written to a bad address");
    }
    Ok(())
}

fn tcp_refusals(server: [u8; 4], web: u16) -> Result<(), &'static str> {
    if TcpStream::connect(server, web + 1, Some(5000)).err() != Some(Errno::Refused) {
        return Err("a connection to a port nobody listens on was not refused");
    }
    // An address on the local network that nobody has.
    let nobody = [server[0], server[1], server[2], 99];
    let before = omit_rt::uptime_ms();
    if TcpStream::connect(nobody, web, Some(5000)).err() != Some(Errno::Unreachable) {
        return Err("a connection to an address nobody has was not reported unreachable");
    }
    if omit_rt::uptime_ms() - before > 3000 {
        return Err("finding out that nobody has an address took far too long");
    }
    let to = omit_rt::abi::net::SocketAddr {
        address: server,
        port: 0,
        reserved: 0,
    };
    // SAFETY: the pointer refers to a live value; port 0 is deliberate.
    let zero = unsafe { omit_rt::syscall(nr::TCP_CONNECT, &raw const to as u64, 1000, 0) };
    if Errno::from_result(zero) != Some(Errno::InvalidArgument) {
        return Err("a connection to port 0 was attempted");
    }
    // SAFETY: deliberately a bad pointer; the kernel must refuse it.
    let bad = unsafe { omit_rt::syscall(nr::TCP_CONNECT, 0x10, 1000, 0) };
    if Errno::from_result(bad) != Some(Errno::Fault) {
        return Err("a connection to an address at a bad pointer was attempted");
    }
    Ok(())
}

fn tcp_listening() -> Result<(), &'static str> {
    let listener = TcpListener::bind(9000).map_err(|_| "cannot listen on port 9000")?;
    if TcpListener::bind(9000).err() != Some(Errno::Busy) {
        return Err("a port was listened on twice");
    }
    if listener.accept(Some(0)).err() != Some(Errno::WouldBlock) {
        return Err("accepting without waiting did not report WouldBlock");
    }
    let before = omit_rt::uptime_ms();
    if listener.accept(Some(120)).err() != Some(Errno::TimedOut) {
        return Err("accepting with a timeout did not report TimedOut");
    }
    if !(110..1000).contains(&(omit_rt::uptime_ms() - before)) {
        return Err("a 120 ms timeout took far too little or too much time");
    }
    if omit_rt::poll(&[listener.fd()], 30, false) != Ok(0) {
        return Err("poll reports a listener without connections as readable");
    }
    // A listener is not a connection.
    let mut byte = [0u8; 1];
    if omit_rt::read(listener.fd(), &mut byte).is_ok()
        || omit_rt::write(listener.fd(), &byte).is_ok()
    {
        return Err("a listener was read or written");
    }
    // SAFETY: plain integer arguments, deliberately out of range.
    let zero = unsafe { omit_rt::syscall(nr::TCP_LISTEN, 0, 0, 0) };
    // SAFETY: as above.
    let huge = unsafe { omit_rt::syscall(nr::TCP_LISTEN, 65536, 0, 0) };
    if Errno::from_result(zero) != Some(Errno::InvalidArgument)
        || Errno::from_result(huge) != Some(Errno::InvalidArgument)
    {
        return Err("an impossible port was listened on");
    }
    drop(listener);
    drop(TcpListener::bind(9000).map_err(|_| "a closed listener's port stayed taken")?);
    Ok(())
}

fn tcp_conversation(server: [u8; 4], web: u16) -> Result<(), &'static str> {
    let stream = TcpStream::connect(server, web, Some(5000)).map_err(|_| "cannot connect")?;
    // Nothing has been asked, so nothing is there to read.
    if omit_rt::poll(&[stream.fd()], 50, false) != Ok(0) {
        return Err("poll reports a silent connection as readable");
    }
    let mut buffer = [0u8; 256];
    if omit_rt::read_no_wait(stream.fd(), &mut buffer) != Ok(None) {
        return Err("reading without waiting did not report WouldBlock");
    }
    stream
        .write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n")
        .map_err(|_| "cannot send")?;
    if omit_rt::poll(&[stream.fd()], 3000, false) != Ok(1) {
        return Err("poll did not report the answer");
    }
    // The whole answer, then the end, and the end again.
    let mut total = 0;
    loop {
        match stream.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(count) => total += count,
            Err(_) => return Err("reading the answer failed"),
        }
    }
    if !buffer[..total].starts_with(b"HTTP/1.1 200 OK\r\n")
        || !buffer[..total].ends_with(b"\r\n\r\nHello from the test server\n")
    {
        return Err("the answer is not what the server sends");
    }
    if stream.read(&mut buffer) != Ok(0) {
        return Err("the end of a connection was not reported again");
    }
    if omit_rt::poll(&[stream.fd()], 0, false) != Ok(1) {
        return Err("poll does not report the end of a connection as readable");
    }
    // After telling the other side that nothing more comes, nothing
    // more can be sent.
    stream.shutdown().map_err(|_| "cannot shut down")?;
    if stream.write_all(b"more").err() != Some(Errno::BrokenPipe) {
        return Err("writing after shutting down was accepted");
    }
    // SAFETY: plain integer argument: a descriptor that is not a connection.
    let not_tcp = unsafe { omit_rt::syscall(nr::TCP_SHUTDOWN, omit_rt::abi::STDOUT, 0, 0) };
    if Errno::from_result(not_tcp) != Some(Errno::BadFileDescriptor) {
        return Err("something that is not a connection was shut down");
    }
    // Several connections at once.
    let first = TcpStream::connect(server, web, Some(5000)).map_err(|_| "cannot connect")?;
    let second = TcpStream::connect(server, web, Some(5000)).map_err(|_| "cannot connect")?;
    second
        .write_all(b"GET /missing HTTP/1.0\r\n\r\n")
        .map_err(|_| "cannot send")?;
    first
        .write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n")
        .map_err(|_| "cannot send")?;
    let count = first.read(&mut buffer).map_err(|_| "cannot read")?;
    if !buffer[..count].starts_with(b"HTTP/1.1 200") {
        return Err("two connections got each other's answers");
    }
    let count = second.read(&mut buffer).map_err(|_| "cannot read")?;
    if !buffer[..count].starts_with(b"HTTP/1.1 404") {
        return Err("two connections got each other's answers");
    }
    Ok(())
}

type Check<'a> = &'a dyn Fn() -> Result<(), &'static str>;

fn main() -> i32 {
    let mut args = omit_rt::args().skip(1);
    let server = args.next().and_then(omit_inet::parse_ipv4);
    let port = args.next().and_then(|port| port.parse::<u16>().ok());
    let web = args.next().and_then(|port| port.parse::<u16>().ok());
    let (Some(server), Some(port), Some(web)) = (server, port, web) else {
        println!("usage: nettest SERVER PORT WEB-PORT");
        return 2;
    };
    let checks: [(&str, Check<'_>); 8] = [
        ("interface", &interface),
        ("ports", &ports),
        ("waiting", &waiting),
        ("refusals", &|| refusals(server, port)),
        ("exchange", &|| exchange(server, port)),
        ("tcp refusals", &|| tcp_refusals(server, web)),
        ("tcp listening", &tcp_listening),
        ("tcp conversation", &|| tcp_conversation(server, web)),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("nettest: {name}: ok"),
            Err(reason) => {
                println!("nettest: {name}: FAILED: {reason}");
                failed += 1;
            }
        }
    }
    if failed == 0 {
        println!("nettest: all checks passed");
        0
    } else {
        1
    }
}
