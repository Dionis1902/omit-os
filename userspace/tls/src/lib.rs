// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS client connections.
//!
//! [`Stream`] wraps a connected byte stream — a TCP connection, for a
//! program — and speaks TLS 1.3 over it: after [`Stream::connect`] has
//! checked that the server is who the name says, what is written is
//! encrypted and what is read was sent by that server and by nobody
//! else.
//!
//! The protocol is the `rustls` library's; the cryptography is the
//! RustCrypto project's, through `rustls-rustcrypto`. This crate only
//! moves bytes between the two and the stream. Two things about that
//! choice have to be said wherever this is used:
//!
//! * `rustls-rustcrypto` is an early release and **has not been
//!   audited**; and programs here are compiled without vector
//!   instructions, so it is also slow.
//! * The random numbers come from the system ([`Platform::random`]),
//!   and are only as good as its source.
//!
//! The server's certificate must lead to one of the authorities in
//! Mozilla's list (the `webpki-roots` crate, as of its release) or to
//! one given by the caller; it must be valid at the time
//! [`Platform::now`] reports and name the server asked for. Nothing
//! checks whether a certificate has been revoked.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{
    AppDataRecord, ConnectionState, EncodeError, EncryptError, InsufficientSizeError,
    UnbufferedStatus,
};
use rustls::{CertificateError, ClientConfig, RootCertStore};

/// The system's random number generator: fills the buffer, and returns
/// `false` if it cannot.
pub type RandomSource = fn(&mut [u8]) -> bool;

/// The [`RandomSource`] in use, as an address; 0 for none.
static RANDOM_SOURCE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Says where random numbers come from. Must be called before the
/// first connection on a system the cryptography library has no source
/// of its own for (Omit OS is one; on such a system connections fail
/// without it).
pub fn set_random_source(source: RandomSource) {
    RANDOM_SOURCE.store(source as usize, core::sync::atomic::Ordering::Relaxed);
}

/// Called by the `getrandom` crate where it has no source of its own.
fn random_from_source(buffer: &mut [u8]) -> Result<(), getrandom::Error> {
    let address = RANDOM_SOURCE.load(core::sync::atomic::Ordering::Relaxed);
    if address == 0 {
        return Err(getrandom::Error::UNSUPPORTED);
    }
    // SAFETY: the only value ever stored besides 0 is a `RandomSource`
    // function pointer, in `set_random_source`; function pointers and
    // `usize` have the same size on every platform Rust supports.
    let source = unsafe { core::mem::transmute::<usize, RandomSource>(address) };
    if source(buffer) {
        Ok(())
    } else {
        Err(getrandom::Error::UNEXPECTED)
    }
}

getrandom::register_custom_getrandom!(random_from_source);

/// What the connection runs over.
pub trait Transport {
    /// Why the transport failed.
    type Error;
    /// Reads at least one byte into `buffer`, waiting if need be;
    /// returns how many, or 0 if the other side has finished.
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
    /// Sends all of `data`.
    fn write_all(&mut self, data: &[u8]) -> Result<(), Self::Error>;
}

/// What the connection needs from the system it runs on.
pub trait Platform: Send + Sync + 'static {
    /// Seconds since 1970-01-01 UTC, if the system knows the time.
    /// Without it no certificate can be checked, and no connection is
    /// made.
    fn now(&self) -> Option<u64>;
}

#[derive(Debug)]
struct Clock<P>(P);

impl<P: Platform + fmt::Debug> TimeProvider for Clock<P> {
    fn current_time(&self) -> Option<UnixTime> {
        self.0
            .now()
            .map(|seconds| UnixTime::since_unix_epoch(Duration::from_secs(seconds)))
    }
}

/// Why a connection could not be made, or broke.
#[derive(Debug)]
pub enum Error<E> {
    /// The stream underneath failed.
    Transport(E),
    /// The other side ended the stream without saying, in the protocol,
    /// that it was done. What was received until then may be cut short
    /// — by a fault, or by someone in between.
    CutOff,
    /// The name is neither a host name nor an address.
    Name,
    /// A certificate given as trusted could not be read.
    Trusted,
    /// The protocol failed; the text says how.
    Tls(String),
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "the connection failed: {error}"),
            Self::CutOff => f.write_str("the connection was cut off"),
            Self::Name => f.write_str("the name is not one a certificate can be checked against"),
            Self::Trusted => f.write_str("the certificate given as trusted cannot be read"),
            Self::Tls(text) => f.write_str(text),
        }
    }
}

/// Says what went wrong in words a person can act on, where there are
/// such words.
fn explain<E>(error: rustls::Error) -> Error<E> {
    let text = match &error {
        rustls::Error::InvalidCertificate(reason) => match reason {
            CertificateError::UnknownIssuer => {
                "the server's certificate is not signed by an authority this system trusts".into()
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                "the server's certificate is for another name".into()
            }
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                "the server's certificate has expired (or this system's clock is wrong)".into()
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                "the server's certificate is not valid yet (or this system's clock is wrong)".into()
            }
            other => format!("the server's certificate was not accepted: {other:?}"),
        },
        rustls::Error::FailedToGetCurrentTime => {
            "this system does not know the date, so no certificate can be checked".into()
        }
        rustls::Error::FailedToGetRandomBytes => "the system gave no random numbers".into(),
        rustls::Error::AlertReceived(alert) => {
            format!("the server refused the connection ({alert:?})")
        }
        rustls::Error::PeerIncompatible(_) | rustls::Error::NoApplicationProtocol => {
            format!("the server and this system have no way of talking in common ({error})")
        }
        other => format!("the encrypted connection failed: {other}"),
    };
    Error::Tls(text)
}

/// The DER certificates in `data`: `data` itself if it is one, or the
/// contents of every `CERTIFICATE` block if it is PEM text. `None` if
/// it is neither, or a block is damaged.
pub fn certificates(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    // DER: a SEQUENCE with a long-form length, as every certificate is.
    if data.starts_with(&[0x30, 0x82]) {
        return Some(vec![data.to_vec()]);
    }
    let text = core::str::from_utf8(data).ok()?;
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let body = &rest[start + BEGIN.len()..];
        let end = body.find(END)?;
        found.push(base64(&body[..end])?);
        rest = &body[end + END.len()..];
    }
    (!found.is_empty()).then_some(found)
}

/// Decodes base64 (RFC 4648), ignoring white space.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count, mut padding) = (0u32, 0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if padding != 0 {
            // Nothing follows padding.
            return None;
        }
        bits = bits << 6 | u32::from(value);
        count += 1;
        if count == 4 {
            out.extend_from_slice(&bits.to_be_bytes()[1..]);
            (bits, count) = (0, 0);
        }
    }
    match (count, padding) {
        (0, 0) => {}
        (2, 2) => out.push((bits >> 4) as u8),
        (3, 1) => out.extend_from_slice(&((bits >> 2) as u16).to_be_bytes()),
        _ => return None,
    }
    Some(out)
}

/// Room for one TLS record, the largest thing that has to be held
/// whole.
const RECORD: usize = 16_384 + 512;
/// The most that is ever held of what the server sent and has not been
/// taken in yet.
const INCOMING_MAX: usize = 4 * RECORD;

/// What a call wants from the connection.
#[derive(Clone, Copy)]
enum Goal<'a> {
    /// The handshake finished.
    Ready,
    /// This sent.
    Write(&'a [u8]),
    /// Something to read, or the end.
    Read,
    /// The other side told that nothing more comes from here.
    Close,
}

/// An encrypted connection to a server whose identity was checked.
pub struct Stream<T: Transport> {
    transport: T,
    connection: UnbufferedClientConnection,
    /// What arrived and has not been processed, in `incoming[..held]`.
    incoming: Vec<u8>,
    held: usize,
    /// What was decrypted and not yet read, in `plain[taken..]`.
    plain: Vec<u8>,
    taken: usize,
    /// The server said it has finished sending.
    finished: bool,
}

impl<T: Transport> Stream<T> {
    /// Makes the connection over `transport` to the server called
    /// `name` (a host name or an IP address in text), which must prove
    /// that it is. `trusted` are certificates of authorities (DER) to
    /// accept besides the built-in list.
    pub fn connect<P: Platform + fmt::Debug>(
        transport: T,
        name: &str,
        trusted: &[Vec<u8>],
        platform: P,
    ) -> Result<Self, Error<T::Error>> {
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        for certificate in trusted {
            roots
                .add(CertificateDer::from(certificate.as_slice()))
                .map_err(|_| Error::Trusted)?;
        }
        let config = ClientConfig::builder_with_details(
            Arc::new(rustls_rustcrypto::provider()),
            Arc::new(Clock(platform)),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(explain)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = ServerName::try_from(name.to_string()).map_err(|_| Error::Name)?;
        let connection =
            UnbufferedClientConnection::new(Arc::new(config), name).map_err(explain)?;
        let mut stream = Self {
            transport,
            connection,
            incoming: vec![0; RECORD],
            held: 0,
            plain: Vec::new(),
            taken: 0,
            finished: false,
        };
        stream.drive(Goal::Ready)?;
        Ok(stream)
    }

    /// Runs the protocol until `goal` is reached.
    fn drive(&mut self, goal: Goal<'_>) -> Result<(), Error<T::Error>> {
        // Handshake messages on their way out.
        let mut outgoing: Vec<u8> = Vec::new();
        loop {
            let Self {
                transport,
                connection,
                incoming,
                held,
                plain,
                finished,
                ..
            } = self;
            let UnbufferedStatus { mut discard, state } =
                connection.process_tls_records(&mut incoming[..*held]);
            let (mut done, mut more) = (false, false);
            match state.map_err(explain)? {
                ConnectionState::ReadTraffic(mut records) => {
                    while let Some(record) = records.next_record() {
                        let AppDataRecord {
                            discard: also,
                            payload,
                        } = record.map_err(explain)?;
                        discard += also;
                        plain.extend_from_slice(payload);
                    }
                    done = matches!(goal, Goal::Read);
                }
                ConnectionState::EncodeTlsData(mut message) => {
                    let start = outgoing.len();
                    outgoing.resize(start + 1024, 0);
                    loop {
                        match message.encode(&mut outgoing[start..]) {
                            Ok(length) => {
                                outgoing.truncate(start + length);
                                break;
                            }
                            Err(EncodeError::InsufficientSize(InsufficientSizeError {
                                required_size,
                            })) => outgoing.resize(start + required_size, 0),
                            Err(EncodeError::AlreadyEncoded) => {
                                return Err(Error::Tls("a message was asked for twice".into()));
                            }
                        }
                    }
                }
                ConnectionState::TransmitTlsData(sent) => {
                    transport.write_all(&outgoing).map_err(Error::Transport)?;
                    outgoing.clear();
                    sent.done();
                }
                ConnectionState::BlockedHandshake => more = true,
                ConnectionState::WriteTraffic(mut writer) => match goal {
                    Goal::Ready => done = true,
                    // Nothing to read was there: wait for some.
                    Goal::Read => more = true,
                    Goal::Write(data) => {
                        let mut record = vec![0u8; RECORD];
                        for piece in data.chunks(16_384) {
                            let length = loop {
                                match writer.encrypt(piece, &mut record) {
                                    Ok(length) => break length,
                                    Err(EncryptError::InsufficientSize(
                                        InsufficientSizeError { required_size },
                                    )) => record.resize(required_size, 0),
                                    Err(EncryptError::EncryptExhausted) => {
                                        return Err(Error::Tls(
                                            "the connection has carried all it safely can".into(),
                                        ));
                                    }
                                }
                            };
                            transport
                                .write_all(&record[..length])
                                .map_err(Error::Transport)?;
                        }
                        done = true;
                    }
                    Goal::Close => {
                        let mut record = [0u8; 128];
                        if let Ok(length) = writer.queue_close_notify(&mut record) {
                            transport
                                .write_all(&record[..length])
                                .map_err(Error::Transport)?;
                        }
                        done = true;
                    }
                },
                ConnectionState::PeerClosed => {
                    *finished = true;
                    done = matches!(goal, Goal::Read);
                }
                ConnectionState::Closed => {
                    *finished = true;
                    done = true;
                }
                // Data before the handshake is something only servers
                // receive; and whatever states the library grows later
                // are not ones this code knows how to serve.
                _ => return Err(Error::Tls("the connection reached an unknown state".into())),
            }
            if discard != 0 {
                incoming.copy_within(discard..*held, 0);
                *held -= discard;
            }
            if done {
                return Ok(());
            }
            if more {
                if *held == incoming.len() {
                    if incoming.len() >= INCOMING_MAX {
                        return Err(Error::Tls(
                            "the server sent more than can be taken in at once".into(),
                        ));
                    }
                    incoming.resize(incoming.len() * 2, 0);
                }
                match transport
                    .read(&mut incoming[*held..])
                    .map_err(Error::Transport)?
                {
                    0 => return Err(Error::CutOff),
                    count => *held += count,
                }
            }
        }
    }

    /// Sends all of `data`, encrypted.
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), Error<T::Error>> {
        if data.is_empty() {
            return Ok(());
        }
        self.drive(Goal::Write(data))
    }

    /// Reads decrypted data into `buffer`, waiting for some if need be.
    /// Returns how many bytes, or 0 once the server has said, in the
    /// protocol, that it has finished. A stream that just ends is an
    /// error ([`Error::CutOff`]): what came before it may be incomplete.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Error<T::Error>> {
        if self.taken == self.plain.len() {
            self.plain.clear();
            self.taken = 0;
            if self.finished {
                return Ok(0);
            }
            self.drive(Goal::Read)?;
        }
        let count = buffer.len().min(self.plain.len() - self.taken);
        buffer[..count].copy_from_slice(&self.plain[self.taken..self.taken + count]);
        self.taken += count;
        Ok(count)
    }

    /// Tells the server that nothing more will be sent. (Reading can go
    /// on.) Failure to say so is not reported: the connection is being
    /// given up anyway.
    pub fn close(&mut self) {
        let _ = self.drive(Goal::Close);
    }

    /// The stream underneath.
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648, section 10.
    #[test]
    fn base64_decodes_the_rfcs_examples_and_refuses_what_is_not_base64() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg==", b"foob"),
            ("Zm9vYmE=", b"fooba"),
            ("Zm9vYmFy", b"foobar"),
            ("Zm9v\nYmFy\r\n", b"foobar"),
            ("+/+/", &[0xFB, 0xFF, 0xBF]),
        ] {
            assert_eq!(base64(text).as_deref(), Some(bytes), "{text:?}");
        }
        for text in ["Z", "Zg=", "Zg===", "Zm9v=", "Zg==Zg==", "Zm9*", "=Zm9"] {
            assert_eq!(base64(text), None, "{text:?}");
        }
    }

    /// The page that lists the authorities for people is generated
    /// (`tools/gen-authorities.py`); it must be generated again when
    /// the list changes.
    #[test]
    fn the_built_in_authorities_are_there_and_the_page_about_them_is_current() {
        let built_in = webpki_roots::TLS_SERVER_ROOTS.len();
        assert!(built_in > 100);
        let page = include_str!("../../../docs/TRUSTED_AUTHORITIES.md");
        let listed = page
            .lines()
            .filter(|line| line.starts_with("| ") && line.contains("| `"))
            .count();
        assert_eq!(listed, built_in, "run tools/gen-authorities.py");
    }

    #[test]
    fn errors_are_said_in_words() {
        let said = |error: rustls::Error| explain::<&str>(error).to_string();
        assert!(
            said(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer
            ))
            .contains("not signed by an authority this system trusts")
        );
        assert!(
            said(rustls::Error::InvalidCertificate(CertificateError::Expired))
                .contains("clock is wrong")
        );
        assert!(said(rustls::Error::FailedToGetCurrentTime).contains("does not know the date"));
        assert_eq!(
            Error::<&str>::Transport("reset").to_string(),
            "the connection failed: reset"
        );
        assert_eq!(
            Error::<&str>::CutOff.to_string(),
            "the connection was cut off"
        );
    }
}
