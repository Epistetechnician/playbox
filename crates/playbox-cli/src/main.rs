use ed25519_dalek::SigningKey;
use playbox_core::history::Log;
use std::io::Read;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("demo") => demo(),
        _ => {
            println!("playbox {} (scaffold)", env!("CARGO_PKG_VERSION"));
            println!("commands: demo   build and verify a sample history log");
            println!("planned:  up | join | status | delegate | verify | revoke (see PRD.md)");
        }
    }
}

fn demo() {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut seed))
        .expect("read /dev/urandom");
    let key = SigningKey::from_bytes(&seed);
    let mut log = Log::new();
    for (kind, payload) in [("session_start", "demo"), ("tool_call", "cargo test"), ("commit", "tree:demo")] {
        log.append(&key, kind, payload.as_bytes(), 0);
    }
    match log.verify() {
        Ok(()) => println!("ok: {} entries verified, head {}", log.entries.len(), hex(&log.head())),
        Err(e) => {
            eprintln!("invalid: {e}");
            std::process::exit(1);
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
