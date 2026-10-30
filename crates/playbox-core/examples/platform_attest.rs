//! Print a signed statement carrying platform evidence, for fixtures and manual checks.
//!
//!   cargo run -p playbox-core --example `platform_attest` -- tpm  <nonce-hex>
//!   cargo run -p playbox-core --example `platform_attest` -- se   <nonce-hex> [helper] [key-file]
//!
//! The session key is derived from the nonce so the output is reproducible for a given TPM state.

use ed25519_dalek::SigningKey;
use playbox_core::attest::{self, PlatformProvider};
use playbox_core::platform::{SecureEnclaveProvider, TpmProvider};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (kind, nonce) = (
        args.get(1).map_or("", String::as_str),
        args.get(2).cloned().unwrap_or_default(),
    );
    let provider: Box<dyn PlatformProvider> = match kind {
        "tpm" => Box::new(TpmProvider::default()),
        "se" => Box::new(SecureEnclaveProvider {
            helper: PathBuf::from(args.get(3).cloned().unwrap_or_else(|| "playbox-se".into())),
            key_file: PathBuf::from(args.get(4).cloned().unwrap_or_else(|| "se.key".into())),
        }),
        _ => {
            eprintln!("usage: platform_attest tpm|se <nonce-hex> [helper key-file]");
            std::process::exit(2);
        }
    };
    let seed = *blake3::hash(nonce.as_bytes()).as_bytes();
    let session = SigningKey::from_bytes(&seed);
    let m = attest::measure_self("docker", "example-policy");
    match attest::new_platform_statement(
        &session,
        &nonce,
        "",
        1_700_000_000_000,
        m,
        provider.as_ref(),
    ) {
        Ok(text) => print!("{text}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
