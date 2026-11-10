// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! The client against a real TLS server (rustls's own, on this
//! machine), with certificates made for the occasion by the `openssl`
//! command, which must be installed.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use omit_tls::{Error, Platform, Stream, Transport, certificates};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const NAME: &str = "test.omit.example";

fn openssl(folder: &Path, arguments: &[&str]) {
    let output = Command::new("openssl")
        .args(arguments)
        .current_dir(folder)
        .output()
        .expect("the openssl command is needed for this test");
    assert!(
        output.status.success(),
        "openssl {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An authority and a server certificate signed by it, made once.
struct Papers {
    folder: PathBuf,
    /// The authority's certificate, DER.
    authority: Vec<u8>,
}

fn papers() -> &'static Papers {
    static PAPERS: OnceLock<Papers> = OnceLock::new();
    PAPERS.get_or_init(|| {
        let folder = std::env::temp_dir().join(format!("omit-tls-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let ec = ["-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256"];
        openssl(
            &folder,
            &[&["genpkey"], &ec[..], &["-out", "ca.key"]].concat(),
        );
        openssl(
            &folder,
            &[
                "req",
                "-x509",
                "-new",
                "-key",
                "ca.key",
                "-sha256",
                "-days",
                "30",
                "-subj",
                "/CN=Omit OS test authority",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign",
                "-out",
                "ca.pem",
            ],
        );
        openssl(
            &folder,
            &[&["genpkey"], &ec[..], &["-out", "server.key"]].concat(),
        );
        openssl(
            &folder,
            &[
                "req",
                "-new",
                "-key",
                "server.key",
                "-subj",
                "/CN=test server",
                "-out",
                "server.csr",
            ],
        );
        fs::write(
            folder.join("server.ext"),
            format!(
                "subjectAltName=DNS:{NAME},IP:127.0.0.1\nbasicConstraints=CA:FALSE\n\
                 keyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n"
            ),
        )
        .unwrap();
        openssl(
            &folder,
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-days",
                "30",
                "-sha256",
                "-extfile",
                "server.ext",
                "-out",
                "server.pem",
            ],
        );
        let authority = certificates(&fs::read(folder.join("ca.pem")).unwrap())
            .expect("the authority's certificate is PEM")
            .remove(0);
        Papers { folder, authority }
    })
}

/// What the test server does once a client has connected.
#[derive(Clone, Copy)]
enum Serve {
    /// Sends back each line it gets with "you said: " in front, until
    /// the client finishes; then finishes properly.
    Echo,
    /// Answers one line, then drops the connection without the
    /// protocol's goodbye.
    CutOff,
    /// Sends this many bytes of a pattern, then finishes properly.
    Send(usize),
    /// Reads until the client finishes and answers how much it got.
    Count,
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index * 7 % 251) as u8).collect()
}

/// Starts a server for one connection; returns its port.
fn server(serve: Serve) -> u16 {
    let papers = papers();
    let certificate: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(papers.folder.join("server.pem"))
            .unwrap()
            .map(Result::unwrap)
            .collect();
    let key = PrivateKeyDer::from_pem_file(papers.folder.join("server.key")).unwrap();
    let config =
        rustls::ServerConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certificate, key)
            .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut tls = rustls::Stream::new(&mut connection, &mut socket);
        // A client that gives up during the handshake is the test's
        // business, not the server's: errors end the thread quietly.
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        let mut read_line = |tls: &mut rustls::Stream<'_, _, _>| -> Option<Vec<u8>> {
            line.clear();
            loop {
                match tls.read(&mut byte) {
                    Ok(1) if byte[0] == b'\n' => return Some(line.clone()),
                    Ok(1) => line.push(byte[0]),
                    _ => return None,
                }
            }
        };
        match serve {
            Serve::Echo => {
                while let Some(line) = read_line(&mut tls) {
                    let _ = tls.write_all(&[b"you said: ", &line[..], b"\n"].concat());
                }
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
            Serve::CutOff => {
                if read_line(&mut tls).is_some() {
                    let _ = tls.write_all(b"half an ans");
                    let _ = tls.flush();
                }
                // The socket is dropped without a goodbye.
            }
            Serve::Send(length) => {
                if read_line(&mut tls).is_some() {
                    let _ = tls.write_all(&pattern(length));
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                }
            }
            Serve::Count => {
                let mut total = 0;
                let mut buffer = [0u8; 4096];
                let mut sum = 0u64;
                loop {
                    match tls.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            total += count;
                            sum += buffer[..count].iter().map(|&b| u64::from(b)).sum::<u64>();
                        }
                    }
                }
                let _ = tls.write_all(format!("{total} {sum}\n").as_bytes());
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        }
    });
    port
}

/// A TCP connection as the client's transport.
struct Tcp(TcpStream);

impl Transport for Tcp {
    type Error = std::io::Error;

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buffer)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.0.write_all(data)
    }
}

/// A system that says it is this many seconds after 1970, or does not
/// know.
#[derive(Debug, Clone, Copy)]
struct At(Option<u64>);

impl Platform for At {
    fn now(&self) -> Option<u64> {
        self.0
    }
}

fn now() -> At {
    At(Some(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    ))
}

fn connect(
    serve: Serve,
    name: &str,
    trusted: &[Vec<u8>],
    at: At,
) -> Result<Stream<Tcp>, Error<std::io::Error>> {
    let socket = TcpStream::connect(("127.0.0.1", server(serve))).unwrap();
    Stream::connect(Tcp(socket), name, trusted, at)
}

fn trusted() -> Vec<Vec<u8>> {
    vec![papers().authority.clone()]
}

fn read_to_end(stream: &mut Stream<Tcp>) -> Result<Vec<u8>, Error<std::io::Error>> {
    let mut all = Vec::new();
    let mut buffer = [0u8; 3000];
    loop {
        match stream.read(&mut buffer)? {
            0 => return Ok(all),
            count => all.extend_from_slice(&buffer[..count]),
        }
    }
}

fn refusal(result: Result<Stream<Tcp>, Error<std::io::Error>>) -> String {
    match result {
        Ok(_) => panic!("the connection was made"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_trusted_server_is_talked_to_and_the_end_is_known() {
    let mut stream = connect(Serve::Echo, NAME, &trusted(), now()).unwrap();
    stream.write_all(b"hello\n").unwrap();
    let mut buffer = [0u8; 64];
    let count = stream.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..count], b"you said: hello\n");
    // A second exchange on the same connection; reading in small
    // pieces gives the same bytes.
    stream.write_all(b"again\n").unwrap();
    let mut answer = Vec::new();
    while !answer.ends_with(b"\n") {
        let mut one = [0u8; 4];
        let count = stream.read(&mut one).unwrap();
        answer.extend_from_slice(&one[..count]);
    }
    assert_eq!(answer, b"you said: again\n");
    // We finish; the server finishes; reading says so, every time.
    stream.close();
    assert_eq!(stream.read(&mut buffer).unwrap(), 0);
    assert_eq!(stream.read(&mut buffer).unwrap(), 0);
}

#[test]
fn the_server_may_be_named_by_its_address() {
    let mut stream = connect(Serve::Echo, "127.0.0.1", &trusted(), now()).unwrap();
    stream.write_all(b"by address\n").unwrap();
    let mut buffer = [0u8; 64];
    let count = stream.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..count], b"you said: by address\n");
}

#[test]
fn a_server_nobody_vouches_for_is_refused() {
    // The built-in authorities do not know the test's own.
    let said = refusal(connect(Serve::Echo, NAME, &[], now()));
    assert!(said.contains("not signed by an authority"), "{said}");
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    for name in ["other.omit.example", "127.0.0.2"] {
        let said = refusal(connect(Serve::Echo, name, &trusted(), now()));
        assert!(said.contains("for another name"), "{name}: {said}");
    }
    // And a name that is no name at all is not even tried.
    let said = refusal(connect(Serve::Echo, "not a name", &trusted(), now()));
    assert!(
        said.contains("not one a certificate can be checked"),
        "{said}"
    );
}

#[test]
fn certificates_are_judged_by_the_systems_clock() {
    let present = now().0.unwrap();
    let year = 365 * 24 * 3600;
    let said = refusal(connect(
        Serve::Echo,
        NAME,
        &trusted(),
        At(Some(present + year)),
    ));
    assert!(said.contains("has expired"), "{said}");
    let said = refusal(connect(
        Serve::Echo,
        NAME,
        &trusted(),
        At(Some(present - year)),
    ));
    assert!(said.contains("not valid yet"), "{said}");
    let said = refusal(connect(Serve::Echo, NAME, &trusted(), At(None)));
    assert!(said.contains("does not know the date"), "{said}");
}

#[test]
fn a_connection_that_just_ends_is_reported_as_cut_off() {
    let mut stream = connect(Serve::CutOff, NAME, &trusted(), now()).unwrap();
    stream.write_all(b"question\n").unwrap();
    // What arrived is delivered; then the end is an error, not an end.
    let mut received = Vec::new();
    let mut buffer = [0u8; 64];
    let error = loop {
        match stream.read(&mut buffer) {
            Ok(0) => panic!("the end was taken for a proper one"),
            Ok(count) => received.extend_from_slice(&buffer[..count]),
            Err(error) => break error,
        }
    };
    assert_eq!(received, b"half an ans");
    assert!(matches!(error, Error::CutOff), "{error}");
}

#[test]
fn a_megabyte_arrives_intact() {
    let length = 1 << 20;
    let mut stream = connect(Serve::Send(length), NAME, &trusted(), now()).unwrap();
    stream.write_all(b"send\n").unwrap();
    let received = read_to_end(&mut stream).unwrap();
    assert_eq!(received.len(), length);
    assert!(received == pattern(length));
}

#[test]
fn a_large_upload_arrives_intact() {
    let data = pattern(300_000);
    let mut stream = connect(Serve::Count, NAME, &trusted(), now()).unwrap();
    // More than one record's worth at once, and a small piece.
    stream.write_all(&data[..250_000]).unwrap();
    stream.write_all(&data[250_000..]).unwrap();
    stream.write_all(&[]).unwrap();
    stream.close();
    let answer = read_to_end(&mut stream).unwrap();
    let sum: u64 = data.iter().map(|&byte| u64::from(byte)).sum();
    assert_eq!(
        String::from_utf8(answer).unwrap(),
        format!("300000 {sum}\n")
    );
}

#[test]
fn trusted_certificates_are_read_from_pem_or_der() {
    let pem = fs::read(papers().folder.join("ca.pem")).unwrap();
    let der = certificates(&pem).unwrap();
    assert_eq!(der.len(), 1);
    assert_eq!(der[0], papers().authority);
    assert!(der[0].starts_with(&[0x30, 0x82]));
    // DER is taken as it is.
    assert_eq!(certificates(&der[0]).unwrap(), der);
    // Two blocks in one file, with text around them.
    let two = [b"notes\n", &pem[..], b"\nmore notes\n", &pem[..]].concat();
    assert_eq!(certificates(&two).unwrap().len(), 2);
    // Neither, or damaged: nothing.
    assert_eq!(certificates(b"hello"), None);
    assert_eq!(certificates(b""), None);
    assert_eq!(certificates(&pem[..pem.len() - 30]), None);
    let damaged = String::from_utf8(pem.clone())
        .unwrap()
        .replacen("MII", "M!I", 1);
    assert_eq!(certificates(damaged.as_bytes()), None);
    // A certificate that is not one is refused when connecting.
    let said = refusal(connect(
        Serve::Echo,
        NAME,
        &[b"\x30\x82junk".to_vec()],
        now(),
    ));
    assert!(said.contains("given as trusted cannot be read"), "{said}");
}
