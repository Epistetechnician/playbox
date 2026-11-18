//! Health probes. A probe answers one question, "is it serving right now?", with a hard time limit, and never panics or blocks for longer than
//! the limit. HTTP is spoken by hand (one request line, read the status line) so the core stays free of a network stack.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The port accepts a connection.
    Tcp(SocketAddr),
    /// `GET path` answers with one of the listed status codes.
    Http {
        addr: SocketAddr,
        path: String,
        ok: Vec<u16>,
    },
}

impl Probe {
    pub fn http(addr: SocketAddr, path: &str, ok: &[u16]) -> Self {
        Probe::Http {
            addr,
            path: path.to_string(),
            ok: ok.to_vec(),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        match self {
            Probe::Tcp(a) | Probe::Http { addr: a, .. } => *a,
        }
    }

    pub fn check(&self, limit: Duration) -> bool {
        match self {
            Probe::Tcp(a) => TcpStream::connect_timeout(a, limit).is_ok(),
            Probe::Http { addr, path, ok } => {
                http_status(*addr, path, limit).is_some_and(|s| ok.contains(&s))
            }
        }
    }
}

/// The status code of `GET path`, or `None` if nothing answered in time or the answer was not HTTP.
pub fn http_status(addr: SocketAddr, path: &str, limit: Duration) -> Option<u16> {
    let mut s = TcpStream::connect_timeout(&addr, limit).ok()?;
    s.set_read_timeout(Some(limit)).ok()?;
    s.set_write_timeout(Some(limit)).ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nUser-Agent: playbox-desktop\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut head = [0u8; 64];
    let mut n = 0;
    while n < head.len() {
        match s.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => {
                n += k;
                if head[..n].contains(&b'\n') {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let line = std::str::from_utf8(&head[..n]).ok()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

#[cfg(test)]
pub(crate) mod testserver {
    //! A tiny in-process server for tests: answers every request with a status that the test can change, or stops answering.
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::Arc;
    use std::thread;

    pub struct Server {
        pub addr: SocketAddr,
        pub status: Arc<AtomicU16>, // 0 = accept and say nothing (a hung service)
    }

    pub fn serve(initial: u16) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let status = Arc::new(AtomicU16::new(initial));
        let st = status.clone();
        thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let code = st.load(Ordering::SeqCst);
                if code == 0 {
                    thread::sleep(std::time::Duration::from_millis(400));
                    continue;
                }
                let mut buf = [0u8; 256];
                let _ = c.read(&mut buf);
                let _ = c.write_all(
                    format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        Server { addr, status }
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::serve;
    use super::*;
    use std::sync::atomic::Ordering;

    const T: Duration = Duration::from_millis(300);

    #[test]
    fn http_probe_accepts_listed_codes_only() {
        let s = serve(200);
        assert!(Probe::http(s.addr, "/x", &[200]).check(T));
        s.status.store(500, Ordering::SeqCst);
        assert!(!Probe::http(s.addr, "/x", &[200]).check(T));
        assert!(Probe::http(s.addr, "/x", &[200, 500]).check(T));
    }

    #[test]
    fn a_hung_service_fails_within_the_limit() {
        let s = serve(0);
        let t0 = std::time::Instant::now();
        assert!(!Probe::http(s.addr, "/", &[200]).check(T));
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_closed_port_fails_fast() {
        let a: SocketAddr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        }; // listener dropped: nothing listens there now
        assert!(!Probe::Tcp(a).check(T));
        assert!(!Probe::http(a, "/", &[200]).check(T));
    }

    #[test]
    fn a_non_http_answer_is_not_healthy() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::Write;
            if let Ok((mut c, _)) = l.accept() {
                let _ = c.write_all(b"SSH-2.0-nope\r\n");
            }
        });
        assert!(!Probe::http(addr, "/", &[200]).check(T));
    }
}
