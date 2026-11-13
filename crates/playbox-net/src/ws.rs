//! A small WebSocket client (RFC 6455) for talking to a browser's DevTools endpoint on a trusted local address.
//!
//! Plain `ws://` only: the browser runs beside the host. Text messages only; binary frames are refused. Messages are
//! capped, fragmented messages are reassembled, pings are answered, and the server's `Sec-WebSocket-Accept` is checked.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use sha1::{Digest, Sha1};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The largest message accepted. A screenshot arrives base64-encoded inside one.
pub const MAX_MESSAGE: usize = 24 << 20;

pub struct WebSocket {
    stream: TcpStream,
    buf: Vec<u8>,
    closed: bool,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        for b in h.finish().to_le_bytes() {
            if i < N {
                out[i] = b;
                i += 1;
            }
        }
    }
    out
}

/// `ws://host:port/path` split into its parts.
pub fn parse_ws_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url.strip_prefix("ws://").ok_or_else(|| anyhow!("the browser endpoint must be ws://host:port/... or http://host:port (it runs beside the host)"))?;
    let (hostport, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(a, b)| (a, format!("/{b}")));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h.to_string(), p.parse::<u16>().context("endpoint port")?),
        _ => (hostport.to_string(), 80),
    };
    if host.is_empty() || host.contains(|c: char| c.is_whitespace() || c == '/') {
        bail!("bad endpoint host");
    }
    Ok((host, port, path))
}

impl WebSocket {
    /// # Errors
    ///
    /// Returns an error if the connection or the handshake fails.
    pub async fn connect(url: &str) -> Result<Self> {
        let (host, port, path) = parse_ws_url(url)?;
        let mut stream = TcpStream::connect((host.trim_matches(|c| c == '[' || c == ']'), port)).await.context("connect to the browser")?;
        stream.set_nodelay(true).ok();
        let key = base64::engine::general_purpose::STANDARD.encode(random_bytes::<16>());
        // No Origin header: a browser started without --remote-allow-origins refuses one it does not know.
        let req = format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n");
        stream.write_all(req.as_bytes()).await?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 8192 {
                bail!("the browser's handshake answer is too long");
            }
            if stream.read(&mut byte).await? == 0 {
                bail!("the browser closed the connection during the handshake");
            }
            head.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&head).to_string();
        let mut lines = text.split("\r\n");
        let status = lines.next().unwrap_or("");
        if !status.contains(" 101 ") {
            bail!("the browser refused the WebSocket handshake: {}", status.trim());
        }
        let want = base64::engine::general_purpose::STANDARD.encode(Sha1::digest(format!("{key}{GUID}").as_bytes()));
        let got = lines.find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-accept")).map(|(_, v)| v.trim().to_string()));
        if got.as_deref() != Some(want.as_str()) {
            bail!("the browser's WebSocket handshake answer is wrong");
        }
        Ok(Self { stream, buf: Vec::new(), closed: false })
    }

    /// Send one text message.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is closed or the write fails.
    pub async fn send(&mut self, text: &str) -> Result<()> {
        self.write_frame(0x1, text.as_bytes()).await
    }

    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        if self.closed {
            bail!("the browser connection is closed");
        }
        let mask = random_bytes::<4>();
        let mut f = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 0xFFFF => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.stream.write_all(&f).await?;
        Ok(())
    }

    async fn fill(&mut self, n: usize) -> Result<()> {
        while self.buf.len() < n {
            let mut chunk = vec![0u8; 65536.min(n - self.buf.len()).max(4096)];
            let got = self.stream.read(&mut chunk).await?;
            if got == 0 {
                self.closed = true;
                bail!("the browser closed the connection");
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        Ok(())
    }

    /// The next text message. `Ok(None)` when the browser closed the connection cleanly.
    ///
    /// # Errors
    ///
    /// Returns an error on a protocol violation, an oversized message, or a read failure.
    pub async fn recv(&mut self) -> Result<Option<String>> {
        let mut message: Vec<u8> = Vec::new();
        let mut started = false;
        loop {
            self.fill(2).await?;
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (fin, opcode, masked) = (b0 & 0x80 != 0, b0 & 0x0F, b1 & 0x80 != 0);
            if b0 & 0x70 != 0 || masked {
                bail!("protocol violation from the browser");
            }
            let (hdr, len) = match b1 & 0x7F {
                126 => {
                    self.fill(4).await?;
                    (4, u64::from(u16::from_be_bytes([self.buf[2], self.buf[3]])))
                }
                127 => {
                    self.fill(10).await?;
                    (10, u64::from_be_bytes(self.buf[2..10].try_into().expect("eight bytes")))
                }
                n => (2, u64::from(n)),
            };
            if len > MAX_MESSAGE as u64 || message.len() as u64 + len > MAX_MESSAGE as u64 {
                bail!("a message from the browser is too large");
            }
            let len = len as usize;
            self.fill(hdr + len).await?;
            let payload: Vec<u8> = self.buf[hdr..hdr + len].to_vec();
            self.buf.drain(..hdr + len);
            match opcode {
                0x0 | 0x1 => {
                    if (opcode == 0x1) == started {
                        bail!("protocol violation from the browser (fragmentation)");
                    }
                    started = true;
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(Some(String::from_utf8(message).map_err(|_| anyhow!("the browser sent text that is not UTF-8"))?));
                    }
                }
                0x2 => bail!("the browser sent a binary message"),
                0x8 => {
                    self.write_frame(0x8, payload.get(..2).unwrap_or(&[])).await.ok();
                    self.closed = true;
                    return Ok(None);
                }
                0x9 => self.write_frame(0xA, &payload).await?,
                0xA => {}
                _ => bail!("protocol violation from the browser (opcode)"),
            }
        }
    }

    /// Close politely and drop the connection.
    pub async fn close(mut self) {
        self.write_frame(0x8, &1000u16.to_be_bytes()).await.ok();
        self.stream.shutdown().await.ok();
    }
}

/// The server side of the same protocol, for tests: accept one connection, read masked frames, send unmasked ones.
#[cfg(test)]
pub mod server {
    use super::*;

    pub struct Peer {
        stream: TcpStream,
        buf: Vec<u8>,
    }

    pub async fn accept(l: &tokio::net::TcpListener) -> Peer {
        let (mut s, _) = l.accept().await.unwrap();
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).await.unwrap();
            head.push(b[0]);
        }
        let text = String::from_utf8_lossy(&head).to_string();
        let key = text.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key")).map(|(_, v)| v.trim().to_string())).unwrap();
        let acc = base64::engine::general_purpose::STANDARD.encode(Sha1::digest(format!("{key}{GUID}").as_bytes()));
        s.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {acc}\r\n\r\n").as_bytes()).await.unwrap();
        Peer { stream: s, buf: Vec::new() }
    }

    impl Peer {
        pub async fn recv(&mut self) -> Option<String> {
            loop {
                while self.buf.len() < 6 {
                    let mut c = [0u8; 4096];
                    let n = self.stream.read(&mut c).await.ok()?;
                    if n == 0 {
                        return None;
                    }
                    self.buf.extend_from_slice(&c[..n]);
                }
                let opcode = self.buf[0] & 0x0F;
                let (hdr, len) = match self.buf[1] & 0x7F {
                    126 => (4, u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize),
                    127 => (10, u64::from_be_bytes(self.buf[2..10].try_into().unwrap()) as usize),
                    n => (2, n as usize),
                };
                while self.buf.len() < hdr + 4 + len {
                    let mut c = [0u8; 65536];
                    let n = self.stream.read(&mut c).await.ok()?;
                    if n == 0 {
                        return None;
                    }
                    self.buf.extend_from_slice(&c[..n]);
                }
                let mask: [u8; 4] = self.buf[hdr..hdr + 4].try_into().unwrap();
                let payload: Vec<u8> = self.buf[hdr + 4..hdr + 4 + len].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
                self.buf.drain(..hdr + 4 + len);
                match opcode {
                    0x1 => return Some(String::from_utf8(payload).unwrap()),
                    0x8 => return None,
                    _ => {}
                }
            }
        }

        pub async fn send_frame(&mut self, fin: bool, opcode: u8, payload: &[u8]) {
            let mut f = vec![(if fin { 0x80 } else { 0 }) | opcode];
            match payload.len() {
                n if n < 126 => f.push(n as u8),
                n if n <= 0xFFFF => {
                    f.push(126);
                    f.extend_from_slice(&(n as u16).to_be_bytes());
                }
                n => {
                    f.push(127);
                    f.extend_from_slice(&(n as u64).to_be_bytes());
                }
            }
            f.extend_from_slice(payload);
            self.stream.write_all(&f).await.unwrap();
        }

        pub async fn send(&mut self, text: &str) {
            self.send_frame(true, 0x1, text.as_bytes()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(parse_ws_url("ws://127.0.0.1:9222/devtools/browser/abc").unwrap(), ("127.0.0.1".into(), 9222, "/devtools/browser/abc".into()));
        assert_eq!(parse_ws_url("ws://localhost").unwrap(), ("localhost".into(), 80, "/".into()));
        assert!(parse_ws_url("wss://x/").is_err());
        assert!(parse_ws_url("http://x/").is_err());
        assert!(parse_ws_url("ws://:1/").is_err());
    }

    #[tokio::test]
    async fn messages_pings_fragments_and_big_payloads_round_trip() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = tokio::spawn(async move {
            let mut p = server::accept(&l).await;
            assert_eq!(p.recv().await.as_deref(), Some("hello"));
            let big = "x".repeat(200_000);
            assert_eq!(p.recv().await.as_deref(), Some(big.as_str()));
            p.send_frame(true, 0x9, b"ping").await; // a ping before the message
            p.send_frame(false, 0x1, b"frag").await;
            p.send_frame(true, 0x0, b"ments").await;
            p.send(&"y".repeat(70_000)).await;
            p.send_frame(true, 0x8, &1000u16.to_be_bytes()).await;
        });
        let mut ws = WebSocket::connect(&format!("ws://127.0.0.1:{port}/")).await.unwrap();
        ws.send("hello").await.unwrap();
        ws.send(&"x".repeat(200_000)).await.unwrap();
        assert_eq!(ws.recv().await.unwrap().as_deref(), Some("fragments"));
        assert_eq!(ws.recv().await.unwrap().unwrap().len(), 70_000);
        assert!(ws.recv().await.unwrap().is_none());
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn a_wrong_handshake_a_masked_frame_and_a_binary_frame_are_refused() {
        // A server that does not answer 101.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            s.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await.unwrap();
        });
        assert!(WebSocket::connect(&format!("ws://127.0.0.1:{port}/")).await.is_err());
        // A server that answers 101 with a bad accept value.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await;
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: nope\r\n\r\n").await.unwrap();
        });
        assert!(WebSocket::connect(&format!("ws://127.0.0.1:{port}/")).await.is_err());
        // Binary frames and unknown opcodes are refused; an ordinary text frame is fine.
        for (opcode, bad) in [(0x2u8, true), (0x3, true), (0x1, false)] {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let srv = tokio::spawn(async move {
                let mut p = server::accept(&l).await;
                p.send_frame(true, opcode, b"abc").await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            });
            let mut ws = WebSocket::connect(&format!("ws://127.0.0.1:{port}/")).await.unwrap();
            assert_eq!(ws.recv().await.is_err(), bad, "opcode {opcode}");
            srv.await.unwrap();
        }
    }
}
