// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Just enough HTTP (RFC 9110, RFC 9112) to fetch something: an
//! `http://` address taken apart, a `GET` request, and the head of the
//! response.
//!
//! The request is HTTP/1.0 with `Connection: close`, so the server sends
//! the body as it is and then closes the connection: the body is
//! everything after the head, and its end is the end of the connection.
//! That leaves out, deliberately: chunked transfer (which a server must
//! not use in answer to HTTP/1.0), persistent connections, redirects
//! (reported, not followed), compression, and `https`.

/// TCP port of HTTP.
pub const PORT: u16 = 80;
/// Longest response head this code looks through.
pub const MAX_HEAD_LEN: usize = 8192;

/// An `http://` address in its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Url<'a> {
    /// Host name or IPv4 address.
    pub host: &'a str,
    /// Port; 80 unless stated.
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: &'a str,
}

/// Why an address is not one this code can fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    /// Not `http://`: another scheme, such as `https://`, or none.
    Scheme,
    /// No host name.
    Host,
    /// The port is not a number between 1 and 65535.
    Port,
    /// Contains a space or a control character.
    Character,
}

impl<'a> Url<'a> {
    /// Takes `http://host[:port][/path]` apart. A fragment (`#...`) is
    /// dropped: it is not sent to servers.
    pub fn parse(text: &'a str) -> Result<Self, UrlError> {
        if text.bytes().any(|b| b <= b' ' || b == 0x7F) {
            return Err(UrlError::Character);
        }
        let scheme_len = "http://".len();
        if text.len() < scheme_len || !text[..scheme_len].eq_ignore_ascii_case("http://") {
            return Err(UrlError::Scheme);
        }
        let rest = &text[scheme_len..];
        let rest = rest.split('#').next().unwrap_or(rest);
        let (authority, path) = match rest.find(['/', '?']) {
            Some(at) => rest.split_at(at),
            None => (rest, "/"),
        };
        // User names and passwords in addresses are not supported.
        if authority.contains('@') {
            return Err(UrlError::Host);
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) if port != 0 => (host, port),
                _ => return Err(UrlError::Port),
            },
            None => (authority, PORT),
        };
        if host.is_empty() {
            return Err(UrlError::Host);
        }
        Ok(Self { host, port, path })
    }
}

/// Writes a `GET` request for `url` at the start of `buffer`; returns
/// its length, or `None` if it does not fit.
pub fn write_get(buffer: &mut [u8], url: &Url<'_>) -> Option<usize> {
    let mut at = 0;
    let mut put = |text: &str| -> Option<()> {
        buffer
            .get_mut(at..at + text.len())?
            .copy_from_slice(text.as_bytes());
        at += text.len();
        Some(())
    };
    put("GET ")?;
    // A query without a path ("http://host?x") is asked for as "/?x".
    if !url.path.starts_with('/') {
        put("/")?;
    }
    put(url.path)?;
    put(" HTTP/1.0\r\nHost: ")?;
    put(url.host)?;
    if url.port != PORT {
        let mut digits = [0u8; 5];
        let mut value = url.port;
        let mut start = digits.len();
        while value > 0 {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        put(":")?;
        put(core::str::from_utf8(&digits[start..]).ok()?)?;
    }
    put("\r\nUser-Agent: omit-fetch\r\nAccept: */*\r\nConnection: close\r\n\r\n")?;
    Some(at)
}

/// The head of a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head<'a> {
    /// The status code: 200 for success.
    pub status: u16,
    /// The server's words for the status ("OK", "Not Found").
    pub reason: &'a str,
    /// The length of the body, if the server states it.
    pub content_length: Option<u64>,
    /// Where the server says to look instead, for a 3xx status.
    pub location: Option<&'a str>,
    /// Bytes of head in the data: the body starts there.
    pub length: usize,
}

/// What looking for a head in received data found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadState<'a> {
    /// The head is complete.
    Complete(Head<'a>),
    /// The blank line that ends the head has not arrived yet.
    Incomplete,
    /// This is not an HTTP response.
    Invalid,
}

/// Looks for a complete response head at the start of `data`, which is
/// everything received so far.
pub fn parse_head(data: &[u8]) -> HeadState<'_> {
    // Enough to tell a response from something else early.
    let prefix = b"HTTP/1.";
    let common = data.len().min(prefix.len());
    if data[..common] != prefix[..common] {
        return HeadState::Invalid;
    }
    let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") else {
        return if data.len() > MAX_HEAD_LEN {
            HeadState::Invalid
        } else {
            HeadState::Incomplete
        };
    };
    let Ok(head) = core::str::from_utf8(&data[..end]) else {
        return HeadState::Invalid;
    };
    let mut lines = head.split("\r\n");
    // "HTTP/1.1 200 OK": version, three digits, optional words.
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return HeadState::Invalid;
    };
    if version.len() != 8 || code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return HeadState::Invalid;
    }
    let Ok(status) = code.parse::<u16>() else {
        return HeadState::Invalid;
    };
    let mut content_length = None;
    let mut location = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return HeadState::Invalid;
        };
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("content-length") {
            match value.parse::<u64>() {
                Ok(length) => content_length = Some(length),
                Err(_) => return HeadState::Invalid,
            }
        } else if name.eq_ignore_ascii_case("location") {
            location = Some(value);
        }
    }
    HeadState::Complete(Head {
        status,
        reason: parts.next().unwrap_or(""),
        content_length,
        location,
        length: end + 4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_taken_apart() {
        let url = |host, port, path| Ok(Url { host, port, path });
        assert_eq!(
            Url::parse("http://example.org"),
            url("example.org", 80, "/")
        );
        assert_eq!(
            Url::parse("http://example.org/"),
            url("example.org", 80, "/")
        );
        assert_eq!(
            Url::parse("HTTP://10.0.2.2:8080/a/b.txt?x=1#part"),
            url("10.0.2.2", 8080, "/a/b.txt?x=1")
        );
        assert_eq!(Url::parse("http://host?q=1"), url("host", 80, "?q=1"));
        assert_eq!(Url::parse("http://host:65535/"), url("host", 65535, "/"));
    }

    #[test]
    fn addresses_that_cannot_be_fetched_are_refused() {
        for (text, error) in [
            ("https://example.org/", UrlError::Scheme),
            ("example.org", UrlError::Scheme),
            ("ftp://example.org", UrlError::Scheme),
            ("http:/", UrlError::Scheme),
            ("", UrlError::Scheme),
            ("http://", UrlError::Host),
            ("http:///path", UrlError::Host),
            ("http://:80/", UrlError::Host),
            ("http://user:secret@host/", UrlError::Host),
            ("http://host:0/", UrlError::Port),
            ("http://host:65536/", UrlError::Port),
            ("http://host:http/", UrlError::Port),
            ("http://host:/", UrlError::Port),
            ("http://host/a b", UrlError::Character),
            ("http://host/a\r\nX: y", UrlError::Character),
        ] {
            assert_eq!(Url::parse(text), Err(error), "{text:?}");
        }
    }

    #[test]
    fn the_request_is_what_a_server_expects() {
        let mut buffer = [0xEEu8; 256];
        let url = Url::parse("http://example.org/index.html").unwrap();
        let length = write_get(&mut buffer, &url).unwrap();
        assert_eq!(
            core::str::from_utf8(&buffer[..length]).unwrap(),
            "GET /index.html HTTP/1.0\r\nHost: example.org\r\nUser-Agent: omit-fetch\r\n\
             Accept: */*\r\nConnection: close\r\n\r\n"
        );
        assert_eq!(buffer[length], 0xEE);
        // A port other than 80 is part of the host; a bare query gets a path.
        let url = Url::parse("http://10.0.2.2:18080?x").unwrap();
        let length = write_get(&mut buffer, &url).unwrap();
        assert!(buffer[..length].starts_with(b"GET /?x HTTP/1.0\r\nHost: 10.0.2.2:18080\r\n"));
        // Every buffer that is too small is refused.
        for size in 0..length {
            assert_eq!(write_get(&mut buffer[..size], &url), None, "{size} bytes");
        }
    }

    const RESPONSE: &[u8] =
        b"HTTP/1.1 200 OK\r\nServer: test\r\nContent-Length: 5\r\nX-Empty:\r\n\r\nhello";

    #[test]
    fn a_response_head_is_understood() {
        let HeadState::Complete(head) = parse_head(RESPONSE) else {
            panic!("not complete");
        };
        assert_eq!((head.status, head.reason), (200, "OK"));
        assert_eq!(head.content_length, Some(5));
        assert_eq!(head.location, None);
        assert_eq!(&RESPONSE[head.length..], b"hello");
    }

    #[test]
    fn other_statuses_and_headers() {
        let head = |data| match parse_head(data) {
            HeadState::Complete(head) => head,
            other => panic!("{other:?}"),
        };
        let found = head(b"HTTP/1.0 404 Not Found\r\n\r\n");
        assert_eq!(
            (found.status, found.reason, found.content_length),
            (404, "Not Found", None)
        );
        let moved = head(b"HTTP/1.1 301 Moved Permanently\r\nLOCATION:  http://b/ \r\n\r\n");
        assert_eq!((moved.status, moved.location), (301, Some("http://b/")));
        // No words after the code.
        assert_eq!(head(b"HTTP/1.1 204\r\n\r\n").reason, "");
        assert_eq!(
            head(b"HTTP/1.1 204 \r\ncontent-length:\t0\r\n\r\n").content_length,
            Some(0)
        );
    }

    #[test]
    fn a_head_that_has_not_all_arrived_is_waited_for() {
        let end = RESPONSE.len() - 5;
        for length in 0..end {
            assert_eq!(
                parse_head(&RESPONSE[..length]),
                HeadState::Incomplete,
                "{length} bytes"
            );
        }
        assert!(matches!(
            parse_head(&RESPONSE[..end]),
            HeadState::Complete(_)
        ));
    }

    #[test]
    fn what_is_not_a_response_is_recognised_as_such() {
        for data in [
            &b"SSH-2.0-OpenSSH\r\n"[..],
            b"H",
            b"<html>",
            b"HTTP/2 200\r\n\r\n",
            b"HTTP/1.1 20 OK\r\n\r\n",
            b"HTTP/1.1 2000 OK\r\n\r\n",
            b"HTTP/1.1 abc OK\r\n\r\n",
            b"HTTP/1.1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 \xFF\r\n\r\n",
        ] {
            let state = parse_head(data);
            // "H" alone could still become a response.
            if data == b"H" {
                assert_eq!(state, HeadState::Incomplete);
            } else {
                assert_eq!(
                    state,
                    HeadState::Invalid,
                    "{:?}",
                    core::str::from_utf8(data)
                );
            }
        }
        // A head that never ends.
        let mut endless = [b'a'; MAX_HEAD_LEN + 1];
        endless[..17].copy_from_slice(b"HTTP/1.1 200 OK\r\n");
        assert_eq!(parse_head(&endless), HeadState::Invalid);
        assert_eq!(parse_head(&endless[..MAX_HEAD_LEN]), HeadState::Incomplete);
    }
}
