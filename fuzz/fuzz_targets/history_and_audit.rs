//! Session histories and audit logs are files that get read back and verified, including ones a peer sent.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_core::history;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(log) = history::text::from_text(text) {
            let _ = log.verify();
        }
        let _ = history::text::entry_from_line(text);
    }
    // The audit log verifier reads from a file.
    let path = std::env::temp_dir().join(format!("playbox-fuzz-audit-{}", std::process::id()));
    if std::fs::write(&path, data).is_ok() {
        let _ = playbox_net::audit::verify(&path);
        let _ = playbox_net::audit::tail(&path, 5);
    }
});
