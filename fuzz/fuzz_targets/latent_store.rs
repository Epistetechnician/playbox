//! The latent mailbox a host keeps in `<state dir>/latent.json` is read back at start. A damaged or edited file must give
//! an empty or partial mailbox, never a panic.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::latent::{LatentPolicy, Mailbox};

fuzz_target!(|data: &[u8]| {
    let path = std::env::temp_dir().join(format!("playbox-fuzz-latent-{}.json", std::process::id()));
    if std::fs::write(&path, data).is_ok() {
        let _ = Mailbox::new(LatentPolicy {
            store: Some(path.clone()),
            ..Default::default()
        });
        let _ = std::fs::remove_file(&path);
    }
});
