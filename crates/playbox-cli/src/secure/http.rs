//! A minimal HTTP/1.1 client for probing the hub over raw sockets.

use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

pub(super) fn lan_ip() -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?; // a UDP connect sends nothing; it only picks the outgoing interface
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

pub struct Resp {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

/// Send raw bytes and read the whole answer. Err is "timeout" (no complete answer in time) or "refused".
pub fn raw(addr: SocketAddr, bytes: &[u8]) -> std::result::Result<Resp, &'static str> {
    // A burst of short connections can use up the machine's ephemeral ports (they linger ~15 s): wait for one rather than call it a refusal.
    let mut tries = 0;
    let mut s = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Err("refused"),
            Err(_) if tries < 60 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(_) => return Err("refused"),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.set_write_timeout(Some(Duration::from_secs(3))).ok();
    let _ = s.write_all(bytes); // the hub may answer and close before reading everything (an oversize body)
    if !framed_exactly(bytes) {
        let _ = s.shutdown(Shutdown::Write); // so a body shorter than its Content-Length ends instead of waiting
    } // (otherwise let the hub close first: the side that closes first keeps the socket's 15 s TIME_WAIT, and a long run would run out of ports)
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 512 * 1024 {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("timeout")
            }
            Err(_) => break, // a reset after some bytes still counts as an answer
        }
    }
    Ok(parse_resp(&buf))
}

/// Whether the request's body is exactly as long as its Content-Length says (or it has none).
pub(super) fn framed_exactly(req: &[u8]) -> bool {
    let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&req[..i]).to_lowercase();
    let body = req.len() - i - 4;
    match head.lines().find_map(|l| l.strip_prefix("content-length:")) {
        Some(v) => v.trim().parse::<usize>().is_ok_and(|n| n == body),
        None => body == 0,
    }
}

pub fn parse_resp(buf: &[u8]) -> Resp {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body) = match split {
        Some(i) => (&buf[..i], buf[i + 4..].to_vec()),
        None => (buf, Vec::new()),
    };
    let head = String::from_utf8_lossy(head).to_lowercase();
    let status = head
        .strip_prefix("http/")
        .and_then(|r| r.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Resp { status, head, body }
}

pub fn build(method: &str, path: &str, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut v = format!("{method} {path} HTTP/1.1\r\n").into_bytes();
    for (k, val) in headers {
        v.extend_from_slice(format!("{k}: {val}\r\n").as_bytes());
    }
    v.extend_from_slice(b"Connection: close\r\n\r\n");
    v.extend_from_slice(body);
    v
}

pub(super) fn h(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

pub(super) fn get(addr: SocketAddr, path: &str, extra: &[(String, String)]) -> Option<Resp> {
    let mut hs = vec![h("Host", &format!("127.0.0.1:{}", addr.port()))];
    hs.extend_from_slice(extra);
    raw(addr, &build("GET", path, &hs, b"")).ok()
}
