// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! TCP (RFC 9293): the segment format and one connection's state.
//!
//! A [`Connection`] is a state machine without I/O and without a clock,
//! like the rest of this crate: segments that arrive are given to
//! [`Connection::on_segment`], [`Connection::poll_transmit`] says what
//! to send next (it is asked until it has nothing more), and the caller
//! passes the time in milliseconds. The application's side is
//! [`Connection::write`], [`Connection::read`] and
//! [`Connection::close`].
//!
//! It is a small TCP, correct towards its peer but not fast:
//!
//! * data is taken only in order; a segment that arrives early is
//!   dropped and acknowledged again, so the peer sends it once more;
//! * what was sent and not acknowledged within the retransmission time
//!   is sent again from the first unacknowledged byte. The time starts
//!   at one second and doubles; it is not measured from the connection;
//! * every data segment is acknowledged at once (no delayed
//!   acknowledgements), and small writes go out as they come (no Nagle);
//! * no congestion control. At most one send buffer ([`BUFFER_LEN`]
//!   bytes) is ever in flight, which bounds the harm;
//! * no options except the maximum segment size: no window scaling
//!   (windows up to 64 KiB minus one; ours is at most the receive
//!   buffer), no selective acknowledgement, no timestamps;
//! * no urgent data; simultaneous opens are not supported;
//! * after both sides have closed, the side that closed first lingers
//!   for [`TIME_WAIT_MS`] — long enough to answer a peer that did not
//!   hear the last acknowledgement — rather than the minutes the RFC
//!   suggests.

use alloc::boxed::Box;
use alloc::vec;

use crate::ipv4::PROTOCOL_TCP;
use crate::{Ipv4Addr, be16, be32, checksum_add, checksum_finish};

/// Length of a header without options.
pub const HEADER_LEN: usize = 20;
/// Bytes each direction of a connection can hold: sent and not yet
/// acknowledged, or received and not yet read.
pub const BUFFER_LEN: usize = 8192;
/// Largest segment payload offered to the peer, and used when the peer
/// states nothing larger: what fits an Ethernet frame.
pub const MSS: u16 = 1460;
/// What the peer is assumed to accept if it does not say (RFC 9293).
const DEFAULT_PEER_MSS: u16 = 536;
/// First retransmission time.
pub const RTO_INITIAL_MS: u64 = 1000;
/// The retransmission time stops doubling here.
const RTO_MAX_MS: u64 = 8000;
/// Retransmissions of the same data before the connection is given up.
pub const MAX_RETRIES: u32 = 6;
/// How long a fully closed connection lingers to answer a repeated FIN:
/// longer than the peer keeps repeating it.
pub const TIME_WAIT_MS: u64 = 30_000;

/// No more data from the sender.
pub const FIN: u8 = 0x01;
/// Synchronise sequence numbers: opens a connection.
pub const SYN: u8 = 0x02;
/// Reset: the connection does not exist or is abandoned.
pub const RST: u8 = 0x04;
/// Push: deliver to the application without waiting for more.
pub const PSH: u8 = 0x08;
/// The acknowledgement number is valid.
pub const ACK: u8 = 0x10;

/// A TCP segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    /// Port of the sender.
    pub source_port: u16,
    /// Port of the recipient.
    pub destination_port: u16,
    /// Sequence number of the first byte (or of the SYN or FIN).
    pub sequence: u32,
    /// The next sequence number the sender expects, if `ACK` is set.
    pub acknowledgement: u32,
    /// `FIN`, `SYN`, `RST`, `PSH`, `ACK`.
    pub flags: u8,
    /// Bytes the sender is prepared to receive.
    pub window: u16,
    /// The maximum segment size option, present only with `SYN`.
    pub mss: Option<u16>,
    /// The data.
    pub payload: &'a [u8],
}

fn pseudo_header_sum(source: Ipv4Addr, destination: Ipv4Addr, length: u16) -> u32 {
    let sum = checksum_add(0, &source.0);
    let sum = checksum_add(sum, &destination.0);
    let sum = checksum_add(sum, &[0, PROTOCOL_TCP]);
    checksum_add(sum, &length.to_be_bytes())
}

impl<'a> Segment<'a> {
    /// Parses the payload of an IPv4 packet from `source` to
    /// `destination`; `None` if it is malformed or its checksum wrong.
    pub fn parse(data: &'a [u8], source: Ipv4Addr, destination: Ipv4Addr) -> Option<Self> {
        if data.len() < HEADER_LEN || data.len() > usize::from(u16::MAX) {
            return None;
        }
        let header_len = usize::from(data[12] >> 4) * 4;
        if header_len < HEADER_LEN || header_len > data.len() {
            return None;
        }
        let sum = checksum_add(
            pseudo_header_sum(source, destination, data.len() as u16),
            data,
        );
        if checksum_finish(sum) != 0 {
            return None;
        }
        let flags = data[13];
        let mut mss = None;
        let mut options = &data[HEADER_LEN..header_len];
        while let Some((&kind, rest)) = options.split_first() {
            match kind {
                0 => break,
                1 => options = rest,
                _ => {
                    let (&length, _) = rest.split_first()?;
                    let length = usize::from(length);
                    if length < 2 || length > options.len() {
                        return None;
                    }
                    if kind == 2 && length == 4 && flags & SYN != 0 {
                        mss = Some(be16(options, 2));
                    }
                    options = &options[length..];
                }
            }
        }
        Some(Self {
            source_port: be16(data, 0),
            destination_port: be16(data, 2),
            sequence: be32(data, 4),
            acknowledgement: be32(data, 8),
            flags,
            window: be16(data, 14),
            mss,
            payload: &data[header_len..],
        })
    }

    /// Writes the segment, for an IPv4 packet from `source` to
    /// `destination`, at the start of `buffer`; returns its length, or
    /// `None` if the buffer is too small.
    pub fn write(
        &self,
        buffer: &mut [u8],
        source: Ipv4Addr,
        destination: Ipv4Addr,
    ) -> Option<usize> {
        let header_len = if self.mss.is_some() {
            HEADER_LEN + 4
        } else {
            HEADER_LEN
        };
        let total = header_len + self.payload.len();
        let length = u16::try_from(total).ok()?;
        let out = buffer.get_mut(..total)?;
        out[0..2].copy_from_slice(&self.source_port.to_be_bytes());
        out[2..4].copy_from_slice(&self.destination_port.to_be_bytes());
        out[4..8].copy_from_slice(&self.sequence.to_be_bytes());
        out[8..12].copy_from_slice(&self.acknowledgement.to_be_bytes());
        out[12] = ((header_len / 4) as u8) << 4;
        out[13] = self.flags;
        out[14..16].copy_from_slice(&self.window.to_be_bytes());
        out[16..20].fill(0);
        if let Some(mss) = self.mss {
            out[20] = 2;
            out[21] = 4;
            out[22..24].copy_from_slice(&mss.to_be_bytes());
        }
        out[header_len..].copy_from_slice(self.payload);
        let sum = checksum_finish(checksum_add(
            pseudo_header_sum(source, destination, length),
            out,
        ));
        out[16..18].copy_from_slice(&sum.to_be_bytes());
        Some(total)
    }

    /// Sequence numbers the segment occupies: its data, plus one each
    /// for `SYN` and `FIN`.
    pub fn sequence_len(&self) -> u32 {
        self.payload.len() as u32
            + u32::from(self.flags & SYN != 0)
            + u32::from(self.flags & FIN != 0)
    }

    /// The reset to answer this segment with when it belongs to no
    /// connection (RFC 9293, 3.10.7.1); `None` for a reset, which is
    /// never answered. The ports are those of the answer.
    pub fn reset_reply(&self) -> Option<Segment<'static>> {
        if self.flags & RST != 0 {
            return None;
        }
        let (sequence, acknowledgement, flags) = if self.flags & ACK != 0 {
            (self.acknowledgement, 0, RST)
        } else {
            (
                0,
                self.sequence.wrapping_add(self.sequence_len()),
                RST | ACK,
            )
        };
        Some(Segment {
            source_port: self.destination_port,
            destination_port: self.source_port,
            sequence,
            acknowledgement,
            flags,
            window: 0,
            mss: None,
            payload: &[],
        })
    }
}

/// `a` comes before `b` in sequence space.
fn before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

/// A fixed-size queue of bytes. The bytes are on the heap, so that a
/// connection is small enough to be moved about.
struct Ring {
    data: Box<[u8]>,
    start: usize,
    len: usize,
}

impl Ring {
    fn new() -> Self {
        Self {
            data: vec![0; BUFFER_LEN].into_boxed_slice(),
            start: 0,
            len: 0,
        }
    }

    fn free(&self) -> usize {
        BUFFER_LEN - self.len
    }

    /// Appends as much of `bytes` as fits; returns how much.
    fn push(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(self.free());
        for (index, &byte) in bytes[..count].iter().enumerate() {
            self.data[(self.start + self.len + index) % BUFFER_LEN] = byte;
        }
        self.len += count;
        count
    }

    /// Copies up to `out.len()` bytes starting `offset` bytes in,
    /// without removing them; returns how many.
    fn peek(&self, offset: usize, out: &mut [u8]) -> usize {
        let count = out.len().min(self.len.saturating_sub(offset));
        for (index, byte) in out[..count].iter_mut().enumerate() {
            *byte = self.data[(self.start + offset + index) % BUFFER_LEN];
        }
        count
    }

    /// Removes the first `count` bytes.
    fn consume(&mut self, count: usize) {
        let count = count.min(self.len);
        self.start = (self.start + count) % BUFFER_LEN;
        self.len -= count;
    }
}

/// Where a connection is in its life (RFC 9293, 3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No connection: never opened, finished, or failed.
    Closed,
    /// Our SYN is out; waiting for the peer's.
    SynSent,
    /// The peer's SYN has been answered; waiting for its acknowledgement.
    SynReceived,
    /// Open in both directions.
    Established,
    /// We have closed; our FIN is not acknowledged yet.
    FinWait1,
    /// We have closed and the peer knows; it may still send.
    FinWait2,
    /// The peer has closed; we may still send.
    CloseWait,
    /// Both have closed at once; waiting for our FIN's acknowledgement.
    Closing,
    /// The peer closed, then we did; waiting for our FIN's
    /// acknowledgement.
    LastAck,
    /// Both directions are closed; lingering briefly.
    TimeWait,
}

/// Why a connection ended other than by both sides closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Nobody listens on the port: the peer answered our SYN with a
    /// reset.
    Refused,
    /// The peer reset the connection.
    Reset,
    /// The peer stopped answering.
    TimedOut,
}

/// One end of a TCP connection.
pub struct Connection {
    state: State,
    error: Option<Error>,
    local_port: u16,
    remote: Ipv4Addr,
    remote_port: u16,

    /// Our initial sequence number: that of our SYN.
    iss: u32,
    /// Oldest sequence number not yet acknowledged.
    snd_una: u32,
    /// Next sequence number to send; moved back to `snd_una` when
    /// retransmitting.
    snd_nxt: u32,
    /// Highest sequence number ever sent, plus one.
    snd_max: u32,
    /// The peer's receive window.
    snd_wnd: u32,
    /// Largest payload the peer accepts.
    peer_mss: u16,
    /// Data written by the application and not yet acknowledged.
    tx: Ring,
    /// Sequence number of the first byte in `tx`.
    tx_seq: u32,
    /// The application has closed: a FIN follows the data.
    fin_queued: bool,

    /// Next sequence number expected from the peer.
    rcv_nxt: u32,
    /// Data received and not yet read by the application.
    rx: Ring,
    /// The peer has sent its FIN (and everything before it has arrived).
    peer_closed: bool,

    /// When what is in flight is sent again, if anything is in flight.
    retransmit_at: Option<u64>,
    rto_ms: u64,
    retries: u32,
    /// Send one byte although the peer's window is closed.
    probe: bool,
    /// An acknowledgement is owed.
    ack_pending: bool,
    /// A reset is to be sent, after which the connection is gone.
    reset_pending: bool,
    /// When a connection in `TimeWait` ends.
    time_wait_until: u64,
}

impl Connection {
    fn new(
        state: State,
        local_port: u16,
        remote: Ipv4Addr,
        remote_port: u16,
        initial_sequence: u32,
    ) -> Self {
        Self {
            state,
            error: None,
            local_port,
            remote,
            remote_port,
            iss: initial_sequence,
            snd_una: initial_sequence,
            snd_nxt: initial_sequence,
            snd_max: initial_sequence,
            snd_wnd: 0,
            peer_mss: DEFAULT_PEER_MSS,
            tx: Ring::new(),
            tx_seq: initial_sequence.wrapping_add(1),
            fin_queued: false,
            rcv_nxt: 0,
            rx: Ring::new(),
            peer_closed: false,
            retransmit_at: None,
            rto_ms: RTO_INITIAL_MS,
            retries: 0,
            probe: false,
            ack_pending: false,
            reset_pending: false,
            time_wait_until: 0,
        }
    }

    /// A connection being opened from `local_port` to `remote`, port
    /// `remote_port`. `initial_sequence` should be hard to guess and
    /// differ from one connection to the next.
    pub fn connect(
        local_port: u16,
        remote: Ipv4Addr,
        remote_port: u16,
        initial_sequence: u32,
    ) -> Self {
        Self::new(
            State::SynSent,
            local_port,
            remote,
            remote_port,
            initial_sequence,
        )
    }

    /// A connection being opened by the peer: `syn` is its opening
    /// segment, received from `remote` on a port being listened on.
    /// `None` if the segment is not a plain SYN.
    pub fn accept(syn: &Segment<'_>, remote: Ipv4Addr, initial_sequence: u32) -> Option<Self> {
        if syn.flags & (SYN | ACK | RST | FIN) != SYN {
            return None;
        }
        let mut connection = Self::new(
            State::SynReceived,
            syn.destination_port,
            remote,
            syn.source_port,
            initial_sequence,
        );
        connection.rcv_nxt = syn.sequence.wrapping_add(1);
        connection.snd_wnd = u32::from(syn.window);
        connection.peer_mss = syn.mss.unwrap_or(DEFAULT_PEER_MSS).min(MSS);
        Some(connection)
    }

    /// Where the connection is in its life.
    pub fn state(&self) -> State {
        self.state
    }

    /// Why the connection failed, if it did.
    pub fn error(&self) -> Option<Error> {
        self.error
    }

    /// The local port.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// The peer's address and port.
    pub fn remote(&self) -> (Ipv4Addr, u16) {
        (self.remote, self.remote_port)
    }

    /// Whether `segment`, received from `from`, belongs to this
    /// connection.
    pub fn matches(&self, segment: &Segment<'_>, from: Ipv4Addr) -> bool {
        self.state != State::Closed
            && from == self.remote
            && segment.source_port == self.remote_port
            && segment.destination_port == self.local_port
    }

    /// Whether the opening handshake has completed (the connection may
    /// have gone further since).
    pub fn is_open(&self) -> bool {
        !matches!(
            self.state,
            State::Closed | State::SynSent | State::SynReceived
        )
    }

    /// Bytes waiting to be read.
    pub fn readable(&self) -> usize {
        self.rx.len
    }

    /// Whether everything the peer will ever send has been read: it has
    /// closed its side, or the connection is gone, and nothing is
    /// waiting.
    pub fn at_end(&self) -> bool {
        self.rx.len == 0 && (self.peer_closed || self.state == State::Closed)
    }

    /// Bytes [`Connection::write`] would accept now.
    pub fn writable(&self) -> usize {
        if matches!(self.state, State::Established | State::CloseWait) && !self.fin_queued {
            self.tx.free()
        } else {
            0
        }
    }

    /// Whether everything written, and the close if there was one, has
    /// been acknowledged by the peer.
    pub fn all_sent(&self) -> bool {
        self.tx.len == 0 && self.snd_una == self.snd_max
    }

    /// Takes received data in order; returns the number of bytes.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let free_before = self.rx.free();
        let count = self.rx.peek(0, out);
        self.rx.consume(count);
        // If the window had shrunk below a segment, tell the peer that
        // there is room again.
        if count > 0 && free_before < usize::from(MSS) && self.rx.free() >= usize::from(MSS) {
            self.ack_pending = true;
        }
        count
    }

    /// Queues data for sending; returns how much of it was taken (less
    /// than all of it when the send buffer is full, nothing once the
    /// connection is closed or closing).
    pub fn write(&mut self, data: &[u8]) -> usize {
        if self.writable() == 0 {
            return 0;
        }
        self.tx.push(data)
    }

    /// Closes our side: what has been written is still delivered, then
    /// the peer is told there is no more. Data from the peer can still
    /// be read.
    pub fn close(&mut self) {
        match self.state {
            State::SynSent => self.state = State::Closed,
            State::SynReceived | State::Established => {
                self.fin_queued = true;
                self.state = State::FinWait1;
            }
            State::CloseWait => {
                self.fin_queued = true;
                self.state = State::LastAck;
            }
            _ => {}
        }
    }

    /// Abandons the connection: the peer is sent a reset and nothing
    /// further is delivered either way.
    pub fn abort(&mut self) {
        match self.state {
            State::Closed => {}
            State::SynSent => self.state = State::Closed,
            _ => self.reset_pending = true,
        }
    }

    fn fail(&mut self, error: Error) {
        self.state = State::Closed;
        self.error = Some(error);
        self.retransmit_at = None;
    }

    /// Sequence number just after the last byte in the send buffer:
    /// where our FIN goes.
    fn tx_end(&self) -> u32 {
        self.tx_seq.wrapping_add(self.tx.len as u32)
    }

    /// The window to advertise.
    fn window(&self) -> u16 {
        self.rx.free().min(usize::from(u16::MAX)) as u16
    }

    /// Takes in a segment for which [`Connection::matches`] holds.
    pub fn on_segment(&mut self, segment: &Segment<'_>, now_ms: u64) {
        match self.state {
            State::Closed => {}
            State::SynSent => self.on_segment_syn_sent(segment),
            _ => self.on_segment_synchronised(segment, now_ms),
        }
    }

    fn on_segment_syn_sent(&mut self, segment: &Segment<'_>) {
        let acks_our_syn =
            segment.flags & ACK != 0 && segment.acknowledgement == self.iss.wrapping_add(1);
        if segment.flags & RST != 0 {
            // Only a reset that acknowledges our SYN is believed.
            if acks_our_syn {
                self.fail(Error::Refused);
            }
            return;
        }
        if segment.flags & SYN == 0 || !acks_our_syn {
            // Something left over from an earlier connection, or an
            // attempt at a simultaneous open: not answered.
            return;
        }
        self.rcv_nxt = segment.sequence.wrapping_add(1);
        self.snd_una = segment.acknowledgement;
        self.snd_nxt = self.snd_una;
        self.snd_wnd = u32::from(segment.window);
        self.peer_mss = segment.mss.unwrap_or(DEFAULT_PEER_MSS).min(MSS);
        self.state = State::Established;
        self.retransmit_at = None;
        self.retries = 0;
        self.rto_ms = RTO_INITIAL_MS;
        self.ack_pending = true;
    }

    fn on_segment_synchronised(&mut self, segment: &Segment<'_>, now_ms: u64) {
        // 1. Is the segment where we are in the peer's sequence space?
        // Data is only taken in order, so what counts is whether the
        // segment starts at, or reaches across, the next byte expected.
        let mut payload = segment.payload;
        let mut sequence = segment.sequence;
        let mut flags = segment.flags;
        if before(sequence, self.rcv_nxt) {
            // Starts with something already received: cut that off.
            let already = self.rcv_nxt.wrapping_sub(sequence);
            let mut cut = already;
            if flags & SYN != 0 {
                flags &= !SYN;
                cut -= 1;
            }
            if cut as usize > payload.len() {
                // Entirely old (a repeated FIN counts as old, too): the
                // peer has not heard our acknowledgement.
                if flags & RST == 0 {
                    self.ack_pending = true;
                    if self.state == State::TimeWait {
                        self.time_wait_until = now_ms + TIME_WAIT_MS;
                    }
                }
                return;
            }
            // What is left — new data, or a FIN that follows data
            // which was all old — starts at the next byte expected.
            payload = &payload[cut as usize..];
            sequence = self.rcv_nxt;
            if payload.is_empty() {
                // Nothing new in it: the peer has not heard that this
                // arrived, so say it again.
                self.ack_pending = true;
            }
        }
        if sequence != self.rcv_nxt {
            // From the future: something in between was lost.
            if flags & RST == 0 {
                self.ack_pending = true;
            }
            return;
        }

        // 2. Reset.
        if flags & RST != 0 {
            if self.state == State::TimeWait {
                // Both sides had finished: the peer has merely forgotten
                // the connection sooner than we have.
                self.state = State::Closed;
            } else {
                self.fail(Error::Reset);
            }
            return;
        }
        // 3. A SYN here is the peer's opening segment again (our answer
        // was lost) and was cut off above, or it is nonsense.
        if flags & SYN != 0 {
            self.ack_pending = true;
            return;
        }
        // 4. Acknowledgement.
        if flags & ACK == 0 {
            return;
        }
        let ack = segment.acknowledgement;
        if before(self.snd_max, ack) {
            // Acknowledges what was never sent.
            self.ack_pending = true;
            return;
        }
        if before(self.snd_una, ack) {
            self.acknowledged(ack, now_ms);
        }
        if !before(ack, self.snd_una) {
            // Not an old acknowledgement: its window is current.
            self.snd_wnd = u32::from(segment.window);
            if self.snd_wnd > 0 {
                self.probe = false;
            } else {
                // The peer is there and has no room: keep asking for
                // as long as it takes.
                self.retries = 0;
            }
        }
        if self.state == State::Closed {
            return;
        }

        // 5. Data.
        let mut taken = 0;
        if !payload.is_empty()
            && matches!(
                self.state,
                State::Established | State::FinWait1 | State::FinWait2
            )
        {
            taken = self.rx.push(payload);
            self.rcv_nxt = self.rcv_nxt.wrapping_add(taken as u32);
            self.ack_pending = true;
        }
        // 6. The peer's FIN, once everything before it is in.
        if flags & FIN != 0 && taken == payload.len() && !self.peer_closed {
            match self.state {
                State::Established => self.state = State::CloseWait,
                State::FinWait1 => self.state = State::Closing,
                State::FinWait2 => {
                    self.state = State::TimeWait;
                    self.time_wait_until = now_ms + TIME_WAIT_MS;
                    self.retransmit_at = None;
                }
                // SynReceived: a FIN before the handshake completed.
                _ => return,
            }
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.peer_closed = true;
            self.ack_pending = true;
        } else if flags & FIN != 0 {
            // A FIN that cannot be taken yet: say where things are.
            self.ack_pending = true;
        }
    }

    /// The peer has acknowledged everything before `ack`, which is
    /// more than before.
    fn acknowledged(&mut self, ack: u32, now_ms: u64) {
        // Data leaves the send buffer. (`ack` may also cover our SYN,
        // before the data, and our FIN, after it.)
        if before(self.tx_seq, ack) {
            let count = (ack.wrapping_sub(self.tx_seq) as usize).min(self.tx.len);
            self.tx.consume(count);
            self.tx_seq = self.tx_seq.wrapping_add(count as u32);
        }
        self.snd_una = ack;
        if before(self.snd_nxt, ack) {
            self.snd_nxt = ack;
        }
        self.retries = 0;
        self.rto_ms = RTO_INITIAL_MS;
        self.retransmit_at = (self.snd_una != self.snd_max).then_some(now_ms + self.rto_ms);

        let fin_acknowledged = self.fin_queued && ack == self.tx_end().wrapping_add(1);
        match self.state {
            State::SynReceived => self.state = State::Established,
            State::FinWait1 if fin_acknowledged => self.state = State::FinWait2,
            State::Closing if fin_acknowledged => {
                self.state = State::TimeWait;
                self.time_wait_until = now_ms + TIME_WAIT_MS;
            }
            State::LastAck if fin_acknowledged => self.state = State::Closed,
            _ => {}
        }
    }

    /// Writes the next segment the connection has to send into
    /// `buffer`, as the payload of an IPv4 packet from `local` to the
    /// peer, and returns its length; `None` when there is nothing to
    /// send at time `now_ms`. To be called again until it returns
    /// `None`, and again whenever time has passed or something has
    /// happened to the connection. `buffer` should hold a full segment
    /// ([`HEADER_LEN`] + 4 + [`MSS`] bytes).
    pub fn poll_transmit(
        &mut self,
        local: Ipv4Addr,
        now_ms: u64,
        buffer: &mut [u8],
    ) -> Option<usize> {
        if self.state == State::Closed {
            return None;
        }
        let mut segment = Segment {
            source_port: self.local_port,
            destination_port: self.remote_port,
            sequence: self.snd_nxt,
            acknowledgement: self.rcv_nxt,
            flags: ACK,
            window: self.window(),
            mss: None,
            payload: &[],
        };
        // What is in flight has gone unanswered for too long.
        if self.retransmit_at.is_some_and(|at| now_ms >= at) {
            if self.retries == MAX_RETRIES {
                self.error = Some(Error::TimedOut);
                if self.state == State::SynSent {
                    self.fail(Error::TimedOut);
                    return None;
                }
                // Tell the peer, in case it is still there after all.
                self.reset_pending = true;
            } else {
                self.retries += 1;
                self.rto_ms = (self.rto_ms * 2).min(RTO_MAX_MS);
                self.retransmit_at = None;
                // Everything unacknowledged is sent again; if the
                // peer's window is closed, one byte goes regardless, to
                // make it say whether that is still so.
                self.snd_nxt = self.snd_una;
                segment.sequence = self.snd_nxt;
                self.probe = self.snd_wnd == 0;
            }
        }
        if self.reset_pending {
            self.state = State::Closed;
            self.retransmit_at = None;
            segment.flags = RST | ACK;
            return segment.write(buffer, local, self.remote);
        }
        if self.state == State::TimeWait && now_ms >= self.time_wait_until {
            self.state = State::Closed;
            return None;
        }

        let mut scratch = [0u8; MSS as usize];
        let length = match self.state {
            State::SynSent | State::SynReceived if self.snd_nxt == self.iss => {
                segment.flags = if self.state == State::SynSent {
                    segment.acknowledgement = 0;
                    SYN
                } else {
                    SYN | ACK
                };
                segment.mss = Some(MSS);
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                self.ack_pending = false;
                segment.write(buffer, local, self.remote)
            }
            State::SynSent | State::SynReceived => None,
            _ => {
                // Data: as much as the peer's window, its segment size
                // and what there is allow.
                let offset = self.snd_nxt.wrapping_sub(self.tx_seq) as usize;
                let unsent = self.tx.len.saturating_sub(offset);
                let in_flight = self.snd_nxt.wrapping_sub(self.snd_una);
                let mut room = self.snd_wnd.saturating_sub(in_flight) as usize;
                if room == 0 && self.probe {
                    room = 1;
                }
                let count = unsent.min(room).min(usize::from(self.peer_mss));
                if count > 0 {
                    self.tx.peek(offset, &mut scratch[..count]);
                    segment.payload = &scratch[..count];
                    if count == unsent {
                        segment.flags |= PSH;
                    }
                }
                // The FIN follows the last byte: with it if this segment
                // carries it, alone if all data has gone. (Once the FIN
                // itself has been sent, `snd_nxt` is past this point.)
                if self.fin_queued && self.snd_nxt.wrapping_add(count as u32) == self.tx_end() {
                    segment.flags |= FIN;
                }
                let occupies = segment.sequence_len();
                if occupies == 0 && unsent > 0 && in_flight == 0 && self.retransmit_at.is_none() {
                    // The window is closed and nothing is in flight to
                    // bring news of it: look again after a while.
                    self.retransmit_at = Some(now_ms + self.rto_ms);
                }
                if occupies > 0 || self.ack_pending {
                    self.snd_nxt = self.snd_nxt.wrapping_add(occupies);
                    self.ack_pending = false;
                    self.probe = false;
                    segment.write(buffer, local, self.remote)
                } else {
                    None
                }
            }
        };
        if before(self.snd_max, self.snd_nxt) {
            self.snd_max = self.snd_nxt;
        }
        if self.snd_una != self.snd_max && self.retransmit_at.is_none() {
            self.retransmit_at = Some(now_ms + self.rto_ms);
        }
        length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::collections::VecDeque;
    use std::vec::Vec;

    const A: Ipv4Addr = Ipv4Addr([10, 0, 2, 15]);
    const B: Ipv4Addr = Ipv4Addr([10, 0, 2, 2]);

    // --- the segment format ---

    #[test]
    fn a_segment_round_trips() {
        for (mss, payload) in [
            (None, &b""[..]),
            (Some(1460), b""),
            (None, b"hello"),
            (None, b"odd"),
        ] {
            let flags = if mss.is_some() { SYN } else { ACK | PSH };
            let segment = Segment {
                source_port: 49152,
                destination_port: 80,
                sequence: 0xDEAD_BEEF,
                acknowledgement: 0x0102_0304,
                flags,
                window: 8192,
                mss,
                payload,
            };
            let mut buffer = [0u8; 64];
            let length = segment.write(&mut buffer, A, B).unwrap();
            assert_eq!(
                length,
                20 + if mss.is_some() { 4 } else { 0 } + payload.len()
            );
            assert_eq!(Segment::parse(&buffer[..length], A, B), Some(segment));
        }
    }

    #[test]
    fn the_bytes_match_a_hand_built_segment() {
        let segment = Segment {
            source_port: 0x1234,
            destination_port: 80,
            sequence: 1,
            acknowledgement: 2,
            flags: ACK | PSH,
            window: 0x2000,
            mss: None,
            payload: b"hi",
        };
        let mut buffer = [0u8; 22];
        segment.write(&mut buffer, A, B).unwrap();
        // Checked with a separate implementation of the checksum:
        // pseudo header 0a00 020f 0a00 0202 0006 0016, then the segment.
        assert_eq!(
            buffer,
            [
                0x12, 0x34, 0, 80, 0, 0, 0, 1, 0, 0, 0, 2, 0x50, 0x18, 0x20, 0, 0xFC, 0xC9, 0, 0,
                b'h', b'i'
            ]
        );
    }

    #[test]
    fn damaged_and_malformed_segments_are_refused() {
        let segment = Segment {
            source_port: 1,
            destination_port: 2,
            sequence: 3,
            acknowledgement: 4,
            flags: SYN,
            window: 5,
            mss: Some(1460),
            payload: b"data",
        };
        let mut buffer = [0u8; 28];
        let length = segment.write(&mut buffer, A, B).unwrap();
        assert_eq!(length, 28);
        for byte in 0..length {
            let mut bad = buffer;
            bad[byte] ^= 0x20;
            assert_eq!(Segment::parse(&bad, A, B), None, "byte {byte}");
        }
        // The checksum covers the addresses.
        assert_eq!(Segment::parse(&buffer, A, Ipv4Addr([10, 0, 2, 3])), None);
        assert_eq!(Segment::parse(&buffer[..19], A, B), None);
        // Every truncation is refused without a panic.
        for cut in 0..length {
            assert_eq!(Segment::parse(&buffer[..cut], A, B), None, "cut at {cut}");
        }
    }

    /// A segment with the given option bytes (padded to a multiple of
    /// four) and a correct checksum.
    fn with_options(options: &[u8], flags: u8) -> Vec<u8> {
        assert_eq!(options.len() % 4, 0);
        let mut bytes = std::vec![0u8; 20];
        bytes[12] = ((5 + options.len() / 4) as u8) << 4;
        bytes[13] = flags;
        bytes.extend_from_slice(options);
        let sum = checksum_finish(checksum_add(
            pseudo_header_sum(A, B, bytes.len() as u16),
            &bytes,
        ));
        bytes[16..18].copy_from_slice(&sum.to_be_bytes());
        bytes
    }

    #[test]
    fn options_are_skipped_and_the_segment_size_found() {
        // No-ops, window scale (kind 3), then the segment size.
        let bytes = with_options(&[1, 1, 3, 3, 7, 1, 2, 4, 0x02, 0x18, 0, 0], SYN);
        assert_eq!(Segment::parse(&bytes, A, B).unwrap().mss, Some(536));
        // Without SYN the option means nothing.
        let bytes = with_options(&[2, 4, 0x02, 0x18], ACK);
        assert_eq!(Segment::parse(&bytes, A, B).unwrap().mss, None);
        // An option that runs past the header; one with an impossible
        // length; one cut off after its kind.
        assert_eq!(
            Segment::parse(&with_options(&[3, 9, 0, 0], SYN), A, B),
            None
        );
        assert_eq!(
            Segment::parse(&with_options(&[3, 1, 0, 0], SYN), A, B),
            None
        );
        assert_eq!(
            Segment::parse(&with_options(&[1, 1, 1, 3], SYN), A, B),
            None
        );
        // End of options: what follows is padding, whatever it holds.
        assert!(Segment::parse(&with_options(&[0, 9, 9, 9], SYN), A, B).is_some());
    }

    #[test]
    fn resets_for_segments_without_a_connection() {
        let base = Segment {
            source_port: 5000,
            destination_port: 80,
            sequence: 100,
            acknowledgement: 900,
            flags: SYN,
            window: 1,
            mss: None,
            payload: b"",
        };
        // A SYN to a closed port: acknowledge it, sequence zero.
        let reply = base.reset_reply().unwrap();
        assert_eq!((reply.source_port, reply.destination_port), (80, 5000));
        assert_eq!(
            (reply.sequence, reply.acknowledgement, reply.flags),
            (0, 101, RST | ACK)
        );
        // A segment with an acknowledgement: reset at that number.
        let reply = Segment {
            flags: ACK,
            payload: b"abc",
            ..base
        }
        .reset_reply()
        .unwrap();
        assert_eq!((reply.sequence, reply.flags), (900, RST));
        // A reset is not answered.
        assert_eq!(Segment { flags: RST, ..base }.reset_reply(), None);
    }

    // --- one connection against a scripted peer ---

    /// Everything the connection has to send right now.
    fn drain(connection: &mut Connection, local: Ipv4Addr, now: u64) -> Vec<Vec<u8>> {
        let mut sent = Vec::new();
        let mut buffer = [0u8; 1500];
        while let Some(length) = connection.poll_transmit(local, now, &mut buffer) {
            sent.push(buffer[..length].to_vec());
            assert!(sent.len() < 100, "the connection never stops sending");
        }
        sent
    }

    fn parse(bytes: &[u8], from: Ipv4Addr, to: Ipv4Addr) -> Segment<'_> {
        Segment::parse(bytes, from, to).expect("the connection sent a bad segment")
    }

    /// The only segment the connection has to send.
    fn one(connection: &mut Connection, now: u64) -> Vec<u8> {
        let mut sent = drain(connection, A, now);
        assert_eq!(sent.len(), 1, "expected exactly one segment");
        sent.remove(0)
    }

    fn from_peer(sequence: u32, acknowledgement: u32, flags: u8, payload: &[u8]) -> Segment<'_> {
        Segment {
            source_port: 80,
            destination_port: 49152,
            sequence,
            acknowledgement,
            flags,
            window: 4096,
            mss: (flags & SYN != 0).then_some(1000),
            payload,
        }
    }

    /// A client connection (initial sequence 1000) to a peer whose
    /// initial sequence is 5000, after the handshake.
    fn established() -> Connection {
        let mut connection = Connection::connect(49152, B, 80, 1000);
        let syn = one(&mut connection, 0);
        let syn = parse(&syn, A, B);
        assert_eq!(syn.flags, SYN);
        assert_eq!((syn.sequence, syn.mss), (1000, Some(MSS)));
        assert_eq!((syn.source_port, syn.destination_port), (49152, 80));
        connection.on_segment(&from_peer(5000, 1001, SYN | ACK, b""), 10);
        assert_eq!(connection.state(), State::Established);
        let ack = one(&mut connection, 10);
        let ack = parse(&ack, A, B);
        assert_eq!(
            (ack.flags, ack.sequence, ack.acknowledgement),
            (ACK, 1001, 5001)
        );
        assert!(drain(&mut connection, A, 10).is_empty());
        connection
    }

    #[test]
    fn the_handshake_opens_a_connection() {
        let connection = established();
        assert!(connection.is_open());
        assert_eq!(connection.error(), None);
        assert_eq!(connection.writable(), BUFFER_LEN);
        assert!(connection.all_sent());
    }

    #[test]
    fn a_reset_to_the_syn_means_refused() {
        let mut connection = Connection::connect(49152, B, 80, 1000);
        one(&mut connection, 0);
        // A reset that does not acknowledge our SYN is not believed.
        connection.on_segment(&from_peer(0, 77, RST | ACK, b""), 5);
        assert_eq!(connection.state(), State::SynSent);
        connection.on_segment(&from_peer(0, 1001, RST | ACK, b""), 5);
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), Some(Error::Refused));
        assert!(drain(&mut connection, A, 10).is_empty());
    }

    #[test]
    fn an_unanswered_syn_is_repeated_then_given_up() {
        let mut connection = Connection::connect(49152, B, 80, 1000);
        let mut times = Vec::new();
        for now in (0..120_000).step_by(100) {
            for bytes in drain(&mut connection, A, now) {
                assert_eq!(parse(&bytes, A, B).flags, SYN);
                times.push(now);
            }
        }
        // One second, then doubling up to eight.
        assert_eq!(times, [0, 1000, 3000, 7000, 15000, 23000, 31000]);
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), Some(Error::TimedOut));
    }

    #[test]
    fn data_is_sent_in_segments_the_peer_accepts_and_leaves_when_acknowledged() {
        let mut connection = established();
        let data: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        assert_eq!(connection.write(&data), 2500);
        let sent = drain(&mut connection, A, 20);
        // The peer's segment size is 1000.
        let lengths: Vec<usize> = sent.iter().map(|s| parse(s, A, B).payload.len()).collect();
        assert_eq!(lengths, [1000, 1000, 500]);
        let first = parse(&sent[0], A, B);
        assert_eq!((first.sequence, first.flags), (1001, ACK));
        let last = parse(&sent[2], A, B);
        assert_eq!((last.sequence, last.flags), (3001, ACK | PSH));
        assert_eq!(last.payload, &data[2000..]);
        assert!(!connection.all_sent());

        // Two segments acknowledged: room for 2000 more bytes.
        assert_eq!(connection.writable(), BUFFER_LEN - 2500);
        connection.on_segment(&from_peer(5001, 3001, ACK, b""), 30);
        assert_eq!(connection.writable(), BUFFER_LEN - 500);
        connection.on_segment(&from_peer(5001, 3501, ACK, b""), 30);
        assert!(connection.all_sent());
        // Nothing more to send, now or after any amount of time.
        assert!(drain(&mut connection, A, 100_000).is_empty());
    }

    #[test]
    fn the_peers_window_is_respected() {
        let mut connection = established();
        // 4096 bytes of window.
        let data = [7u8; 6000];
        assert_eq!(connection.write(&data), 6000);
        let sent = drain(&mut connection, A, 20);
        let total: usize = sent.iter().map(|s| parse(s, A, B).payload.len()).sum();
        assert_eq!(total, 4096);
        // The window moves as data is acknowledged.
        connection.on_segment(&from_peer(5001, 1001 + 4096, ACK, b""), 30);
        let sent = drain(&mut connection, A, 30);
        let total: usize = sent.iter().map(|s| parse(s, A, B).payload.len()).sum();
        assert_eq!(total, 6000 - 4096);
    }

    #[test]
    fn lost_data_is_sent_again_from_the_first_unacknowledged_byte() {
        let mut connection = established();
        connection.write(&[1u8; 1500]);
        assert_eq!(drain(&mut connection, A, 100).len(), 2);
        // The first segment arrived, the second did not.
        connection.on_segment(&from_peer(5001, 2001, ACK, b""), 150);
        assert!(
            drain(&mut connection, A, 1100).is_empty(),
            "the timer restarted at 150"
        );
        let again = one(&mut connection, 1150);
        let again = parse(&again, A, B);
        assert_eq!((again.sequence, again.payload.len()), (2001, 500));
        connection.on_segment(&from_peer(5001, 2501, ACK, b""), 1200);
        assert!(connection.all_sent());
        assert!(drain(&mut connection, A, 60_000).is_empty());
    }

    #[test]
    fn a_peer_that_stops_answering_ends_the_connection() {
        let mut connection = established();
        connection.write(b"anyone?");
        let mut data = 0;
        let mut reset = 0;
        for now in (0..200_000).step_by(100) {
            for bytes in drain(&mut connection, A, now) {
                let segment = parse(&bytes, A, B);
                if segment.flags & RST != 0 {
                    reset += 1;
                } else {
                    assert_eq!(segment.payload, b"anyone?");
                    data += 1;
                }
            }
        }
        assert_eq!((data, reset), (1 + MAX_RETRIES, 1));
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), Some(Error::TimedOut));
        assert!(connection.at_end());
    }

    #[test]
    fn received_data_is_delivered_in_order_and_acknowledged() {
        let mut connection = established();
        connection.on_segment(&from_peer(5001, 1001, ACK | PSH, b"hello "), 20);
        let ack = one(&mut connection, 20);
        let ack = parse(&ack, A, B);
        assert_eq!((ack.acknowledgement, ack.payload.len()), (5007, 0));
        assert_eq!(usize::from(ack.window), BUFFER_LEN - 6);

        // The next segment is lost; the one after it arrives early and
        // is not taken, but answered with what is expected.
        connection.on_segment(&from_peer(5013, 1001, ACK, b"!"), 30);
        assert_eq!(parse(&one(&mut connection, 30), A, B).acknowledgement, 5007);
        // A repeat of old data is acknowledged again and not delivered twice.
        connection.on_segment(&from_peer(5001, 1001, ACK, b"hello "), 40);
        assert_eq!(parse(&one(&mut connection, 40), A, B).acknowledgement, 5007);
        // A segment that overlaps what has arrived: the new part is taken.
        connection.on_segment(&from_peer(5004, 1001, ACK, b"lo world"), 50);
        assert_eq!(parse(&one(&mut connection, 50), A, B).acknowledgement, 5012);

        assert_eq!(connection.readable(), 11);
        let mut buffer = [0u8; 32];
        let count = connection.read(&mut buffer[..5]);
        assert_eq!(&buffer[..count], b"hello");
        let count = connection.read(&mut buffer);
        assert_eq!(&buffer[..count], b" world");
        assert_eq!(connection.read(&mut buffer), 0);
        assert!(!connection.at_end(), "the peer has not closed");
    }

    #[test]
    fn a_full_receive_buffer_closes_the_window_and_reading_reopens_it() {
        let mut connection = established();
        let chunk = [9u8; 1000];
        let mut sequence = 5001u32;
        for _ in 0..9 {
            connection.on_segment(&from_peer(sequence, 1001, ACK, &chunk), 20);
            let ack = one(&mut connection, 20);
            sequence = parse(&ack, A, B).acknowledgement;
        }
        // Eight segments and 192 bytes of the ninth fitted.
        assert_eq!(sequence, 5001 + BUFFER_LEN as u32);
        assert_eq!(connection.readable(), BUFFER_LEN);
        connection.on_segment(&from_peer(sequence, 1001, ACK, &chunk), 30);
        let ack = one(&mut connection, 30);
        let ack = parse(&ack, A, B);
        assert_eq!((ack.acknowledgement, ack.window), (sequence, 0));

        // Reading a little says nothing; reading a segment's worth does.
        let mut buffer = [0u8; 4096];
        connection.read(&mut buffer[..100]);
        assert!(drain(&mut connection, A, 40).is_empty());
        connection.read(&mut buffer[..2000]);
        let update = one(&mut connection, 40);
        assert_eq!(parse(&update, A, B).window, 2100);
    }

    #[test]
    fn a_closed_window_is_probed_until_it_opens() {
        let mut connection = established();
        // The peer acknowledges and closes its window.
        let mut closed = from_peer(5001, 1001, ACK, b"");
        closed.window = 0;
        connection.on_segment(&closed, 20);
        connection.write(b"waiting");
        assert!(drain(&mut connection, A, 30).is_empty());
        // After the retransmission time, one byte goes out; the peer
        // says the window is still closed. That repeats without the
        // connection giving up.
        let mut probes = 0;
        for now in (0..300_000).step_by(100) {
            for bytes in drain(&mut connection, A, now) {
                let segment = parse(&bytes, A, B);
                assert_eq!(segment.payload, b"w");
                probes += 1;
                connection.on_segment(&closed, now);
            }
        }
        assert!(probes > MAX_RETRIES * 2, "{probes} probes");
        assert_eq!(connection.state(), State::Established);
        // The window opens, and the peer has taken the byte of the last
        // probe: the rest goes.
        connection.on_segment(&from_peer(5001, 1002, ACK, b""), 300_000);
        let sent = one(&mut connection, 300_000);
        assert_eq!(parse(&sent, A, B).payload, b"aiting");
    }

    #[test]
    fn we_close_first() {
        let mut connection = established();
        connection.write(b"bye");
        connection.close();
        assert_eq!(connection.state(), State::FinWait1);
        assert_eq!(
            connection.write(b"more"),
            0,
            "nothing is taken after closing"
        );
        // The FIN travels with the last data.
        let sent = one(&mut connection, 20);
        let sent = parse(&sent, A, B);
        assert_eq!((sent.flags, sent.payload), (ACK | PSH | FIN, &b"bye"[..]));
        // Data acknowledged but not the FIN: still waiting.
        connection.on_segment(&from_peer(5001, 1004, ACK, b""), 30);
        assert_eq!(connection.state(), State::FinWait1);
        connection.on_segment(&from_peer(5001, 1005, ACK, b""), 30);
        assert_eq!(connection.state(), State::FinWait2);
        assert!(connection.all_sent());
        // The peer can still send.
        connection.on_segment(&from_peer(5001, 1005, ACK, b"last words"), 40);
        assert_eq!(parse(&one(&mut connection, 40), A, B).acknowledgement, 5011);
        connection.on_segment(&from_peer(5011, 1005, ACK | FIN, b""), 50);
        assert_eq!(connection.state(), State::TimeWait);
        assert_eq!(parse(&one(&mut connection, 50), A, B).acknowledgement, 5012);
        let mut buffer = [0u8; 32];
        assert_eq!(connection.read(&mut buffer), 10);
        assert!(connection.at_end());
        // A repeated FIN (our acknowledgement was lost) is answered.
        connection.on_segment(&from_peer(5011, 1005, ACK | FIN, b""), 60);
        assert_eq!(parse(&one(&mut connection, 60), A, B).acknowledgement, 5012);
        // Then the connection goes away by itself.
        assert!(drain(&mut connection, A, 59 + TIME_WAIT_MS).is_empty());
        assert_eq!(
            connection.state(),
            State::TimeWait,
            "the repeated FIN restarted the wait"
        );
        assert!(drain(&mut connection, A, 60 + TIME_WAIT_MS).is_empty());
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), None);
    }

    #[test]
    fn the_peer_closes_first() {
        let mut connection = established();
        connection.on_segment(&from_peer(5001, 1001, ACK | FIN, b"all"), 20);
        assert_eq!(connection.state(), State::CloseWait);
        assert_eq!(parse(&one(&mut connection, 20), A, B).acknowledgement, 5005);
        let mut buffer = [0u8; 8];
        assert!(!connection.at_end());
        assert_eq!(connection.read(&mut buffer), 3);
        assert!(connection.at_end());
        // We can still send.
        assert_eq!(connection.write(b"reply"), 5);
        connection.close();
        assert_eq!(connection.state(), State::LastAck);
        let sent = one(&mut connection, 30);
        let sent = parse(&sent, A, B);
        assert_eq!((sent.flags, sent.payload), (ACK | PSH | FIN, &b"reply"[..]));
        connection.on_segment(&from_peer(5005, 1007, ACK, b""), 40);
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), None);
    }

    #[test]
    fn both_close_at_once() {
        let mut connection = established();
        connection.close();
        let fin = one(&mut connection, 20);
        assert_eq!(parse(&fin, A, B).flags, ACK | FIN);
        // The peer's FIN crosses ours: it does not acknowledge it.
        connection.on_segment(&from_peer(5001, 1001, ACK | FIN, b""), 20);
        assert_eq!(connection.state(), State::Closing);
        assert_eq!(parse(&one(&mut connection, 20), A, B).acknowledgement, 5002);
        connection.on_segment(&from_peer(5002, 1002, ACK, b""), 30);
        assert_eq!(connection.state(), State::TimeWait);
    }

    #[test]
    fn a_lost_fin_is_sent_again() {
        let mut connection = established();
        connection.close();
        assert_eq!(parse(&one(&mut connection, 0), A, B).flags, ACK | FIN);
        assert!(drain(&mut connection, A, 900).is_empty());
        let again = one(&mut connection, 1000);
        let again = parse(&again, A, B);
        assert_eq!((again.flags, again.sequence), (ACK | FIN, 1001));
    }

    #[test]
    fn a_reset_ends_the_connection_only_at_the_right_sequence_number() {
        let mut connection = established();
        connection.on_segment(&from_peer(6000, 1001, RST, b""), 20);
        assert_eq!(connection.state(), State::Established);
        assert!(
            drain(&mut connection, A, 20).is_empty(),
            "a reset is never answered"
        );
        connection.on_segment(&from_peer(5001, 1001, RST, b""), 20);
        assert_eq!(connection.state(), State::Closed);
        assert_eq!(connection.error(), Some(Error::Reset));
        assert!(connection.at_end());
        assert_eq!(connection.write(b"x"), 0);
    }

    #[test]
    fn abort_sends_a_reset() {
        let mut connection = established();
        connection.write(b"never mind");
        connection.abort();
        let sent = drain(&mut connection, A, 20);
        assert_eq!(sent.len(), 1);
        assert_eq!(parse(&sent[0], A, B).flags, RST | ACK);
        assert_eq!(connection.state(), State::Closed);
    }

    #[test]
    fn acknowledgements_of_unsent_data_are_not_believed() {
        let mut connection = established();
        connection.write(b"abc");
        drain(&mut connection, A, 20);
        connection.on_segment(&from_peer(5001, 9999, ACK, b""), 30);
        assert!(!connection.all_sent());
        // It is answered with where things really are.
        let answer = one(&mut connection, 30);
        let answer = parse(&answer, A, B);
        assert_eq!((answer.sequence, answer.acknowledgement), (1004, 5001));
    }

    // --- passive open ---

    #[test]
    fn a_listener_accepts_a_connection() {
        let syn = Segment {
            source_port: 40000,
            destination_port: 7,
            sequence: 300,
            acknowledgement: 0,
            flags: SYN,
            window: 1000,
            mss: None,
            payload: b"",
        };
        let mut connection = Connection::accept(&syn, B, 9000).unwrap();
        assert_eq!(connection.state(), State::SynReceived);
        assert_eq!(
            (connection.local_port(), connection.remote()),
            (7, (B, 40000))
        );
        assert!(connection.matches(&syn, B));
        assert!(!connection.matches(&syn, A));
        let answer = one(&mut connection, 0);
        let answer = parse(&answer, A, B);
        assert_eq!(answer.flags, SYN | ACK);
        assert_eq!(
            (answer.sequence, answer.acknowledgement, answer.mss),
            (9000, 301, Some(MSS))
        );
        assert!(!connection.is_open());
        // The SYN again (our answer was lost) changes nothing; the
        // answer is repeated by the timer.
        connection.on_segment(&syn, 500);
        assert!(drain(&mut connection, A, 500).is_empty());
        assert_eq!(parse(&one(&mut connection, 1000), A, B).flags, SYN | ACK);

        let mut ack = syn;
        ack.sequence = 301;
        ack.acknowledgement = 9001;
        ack.flags = ACK;
        ack.payload = b"ping";
        connection.on_segment(&ack, 1100);
        assert_eq!(connection.state(), State::Established);
        let mut buffer = [0u8; 8];
        assert_eq!(connection.read(&mut buffer), 4);
        // The peer stated no segment size: 536 is assumed.
        connection.write(&[0u8; 900]);
        let sent = drain(&mut connection, A, 1100);
        let lengths: Vec<usize> = sent.iter().map(|s| parse(s, A, B).payload.len()).collect();
        assert_eq!(lengths, [536, 364]);

        // Only a plain SYN opens a connection.
        for flags in [ACK, SYN | ACK, RST, SYN | FIN, 0] {
            assert!(Connection::accept(&Segment { flags, ..syn }, B, 1).is_none());
        }
    }

    // --- two connections talking through a bad network ---

    /// A deterministic source of "random" numbers.
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % sides
        }
    }

    struct Conversation {
        client: Connection,
        server: Option<Connection>,
        /// Segments on their way, with their arrival time.
        to_server: VecDeque<(u64, Vec<u8>)>,
        to_client: VecDeque<(u64, Vec<u8>)>,
        dice: Dice,
        /// One segment in this many is lost; 0 for none.
        loss: u64,
        /// One in this many is delivered twice; 0 for none.
        duplicate: u64,
        /// Segments take between 1 ms and this long, so they overtake
        /// one another.
        jitter: u64,
        now: u64,
    }

    impl Conversation {
        fn new(seed: u64, loss: u64, duplicate: u64, jitter: u64) -> Self {
            Self {
                client: Connection::connect(49152, B, 7, seed as u32),
                server: None,
                to_server: VecDeque::new(),
                to_client: VecDeque::new(),
                dice: Dice(seed),
                loss,
                duplicate,
                jitter,
                now: 0,
            }
        }

        fn post(&mut self, to_server: bool, bytes: Vec<u8>) {
            if self.loss != 0 && self.dice.roll(self.loss) == 0 {
                return;
            }
            let copies = if self.duplicate != 0 && self.dice.roll(self.duplicate) == 0 {
                2
            } else {
                1
            };
            for _ in 0..copies {
                let arrival = self.now + 1 + self.dice.roll(self.jitter.max(1));
                let queue = if to_server {
                    &mut self.to_server
                } else {
                    &mut self.to_client
                };
                queue.push_back((arrival, bytes.clone()));
            }
        }

        /// One millisecond of the world.
        fn step(&mut self) {
            self.now += 1;
            let now = self.now;
            // Deliveries that are due, in whatever order they come up.
            let due: Vec<Vec<u8>> = {
                let (ready, later): (Vec<_>, Vec<_>) =
                    self.to_server.drain(..).partition(|(at, _)| *at <= now);
                self.to_server = later.into();
                ready.into_iter().map(|(_, bytes)| bytes).collect()
            };
            for bytes in due {
                let segment = Segment::parse(&bytes, A, B).unwrap();
                match &mut self.server {
                    Some(server) if server.matches(&segment, A) => server.on_segment(&segment, now),
                    // The server listens on port 7, for one connection.
                    _ => {
                        let accepted = if self.server.is_none() {
                            Connection::accept(&segment, A, 77_000)
                        } else {
                            None
                        };
                        if let Some(server) = accepted {
                            self.server = Some(server);
                        } else if let Some(reset) = segment.reset_reply() {
                            let mut buffer = [0u8; 64];
                            let length = reset.write(&mut buffer, B, A).unwrap();
                            self.post(false, buffer[..length].to_vec());
                        }
                    }
                }
            }
            let due: Vec<Vec<u8>> = {
                let (ready, later): (Vec<_>, Vec<_>) =
                    self.to_client.drain(..).partition(|(at, _)| *at <= now);
                self.to_client = later.into();
                ready.into_iter().map(|(_, bytes)| bytes).collect()
            };
            for bytes in due {
                let segment = Segment::parse(&bytes, B, A).unwrap();
                if self.client.matches(&segment, B) {
                    self.client.on_segment(&segment, now);
                } else if let Some(reset) = segment.reset_reply() {
                    let mut buffer = [0u8; 64];
                    let length = reset.write(&mut buffer, A, B).unwrap();
                    self.post(true, buffer[..length].to_vec());
                }
            }
            for bytes in drain(&mut self.client, A, now) {
                self.post(true, bytes);
            }
            if let Some(server) = &mut self.server {
                for bytes in drain(server, B, now) {
                    self.post(false, bytes);
                }
            }
        }
    }

    /// The client sends `up` bytes and closes; the server sends `down`
    /// bytes and closes when it has read everything. Returns what each
    /// received.
    fn transfer(mut conversation: Conversation, up: usize, down: usize) -> (Vec<u8>, Vec<u8>) {
        let upload: Vec<u8> = (0..up).map(|i| (i * 7 + i / 251) as u8).collect();
        let download: Vec<u8> = (0..down).map(|i| (i * 13 + i / 241) as u8).collect();
        let (mut sent_up, mut sent_down) = (0, 0);
        let (mut got_up, mut got_down) = (Vec::new(), Vec::new());
        let mut client_closed = false;
        let mut server_closed = false;
        let mut buffer = [0u8; 700];
        for _ in 0..3_000_000u32 {
            conversation.step();
            let client = &mut conversation.client;
            sent_up += client.write(&upload[sent_up..]);
            if sent_up == up && client.is_open() && !client_closed {
                client.close();
                client_closed = true;
            }
            loop {
                let count = client.read(&mut buffer);
                if count == 0 {
                    break;
                }
                got_down.extend_from_slice(&buffer[..count]);
            }
            if let Some(server) = &mut conversation.server {
                sent_down += server.write(&download[sent_down..]);
                loop {
                    let count = server.read(&mut buffer);
                    if count == 0 {
                        break;
                    }
                    got_up.extend_from_slice(&buffer[..count]);
                }
                if sent_down == down && server.at_end() && !server_closed {
                    server.close();
                    server_closed = true;
                }
                if server_closed
                    && server.state() == State::Closed
                    && conversation.client.state() == State::Closed
                {
                    // The data must be whole in any case. The last
                    // acknowledgement of the closing exchange is the
                    // one thing nobody confirms: on a network that
                    // loses it and every repeat of the FIN, the side
                    // that closed last gives up, having delivered and
                    // received everything.
                    let server_error = server.error();
                    assert!(
                        server_error.is_none()
                            || (conversation.loss != 0
                                && conversation.loss <= 3
                                && server_error == Some(Error::TimedOut)),
                        "server: {server_error:?} at {} ms",
                        conversation.now
                    );
                    assert_eq!(conversation.client.error(), None);
                    assert_eq!(got_up, upload, "the upload arrived damaged");
                    assert_eq!(got_down, download, "the download arrived damaged");
                    return (got_up, got_down);
                }
            }
        }
        panic!(
            "no end after {} ms: client {:?}, server {:?}, {} of {up} up, {} of {down} down",
            conversation.now,
            conversation.client.state(),
            conversation.server.as_ref().map(Connection::state),
            got_up.len(),
            got_down.len()
        );
    }

    #[test]
    fn two_connections_exchange_data_over_a_perfect_network() {
        let (up, down) = transfer(Conversation::new(1, 0, 0, 1), 100_000, 30_000);
        assert_eq!((up.len(), down.len()), (100_000, 30_000));
        // Nothing to say, either way.
        transfer(Conversation::new(2, 0, 0, 1), 0, 0);
        transfer(Conversation::new(3, 0, 0, 1), 1, 0);
        transfer(Conversation::new(4, 0, 0, 1), 0, 1);
    }

    #[test]
    fn data_survives_loss_duplication_and_reordering() {
        for seed in 1..=40u64 {
            // One segment in eight lost, one in five doubled, delays of
            // up to 30 ms so that segments overtake each other.
            transfer(Conversation::new(seed, 8, 5, 30), 20_000, 9_000);
        }
        // Heavier loss, including of the opening and closing segments.
        for seed in 100..=120u64 {
            transfer(Conversation::new(seed, 3, 0, 5), 3_000, 3_000);
        }
    }

    #[test]
    fn sequence_numbers_wrap_around() {
        // Initial sequence numbers just below the wrap, on both sides.
        let mut conversation = Conversation::new(5, 6, 0, 10);
        conversation.client = Connection::connect(49152, B, 7, u32::MAX - 500);
        transfer(conversation, 30_000, 2_000);
    }

    #[test]
    fn a_connection_to_a_port_nobody_listens_on_is_refused() {
        let mut conversation = Conversation::new(9, 0, 0, 1);
        conversation.client = Connection::connect(49152, B, 8, 1);
        // The "server" only accepts on port 7; give it something else to do.
        for _ in 0..50 {
            conversation.now += 1;
            let now = conversation.now;
            for bytes in drain(&mut conversation.client, A, now) {
                let segment = Segment::parse(&bytes, A, B).unwrap();
                let reset = segment.reset_reply().unwrap();
                let mut buffer = [0u8; 64];
                let length = reset.write(&mut buffer, B, A).unwrap();
                let reply = Segment::parse(&buffer[..length], B, A).unwrap();
                assert!(conversation.client.matches(&reply, B));
                conversation.client.on_segment(&reply, now);
            }
        }
        assert_eq!(conversation.client.error(), Some(Error::Refused));
    }
}
