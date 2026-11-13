//! The calls and conversations files a host reads back at start. A damaged or hand-edited state directory must be refused with an
//! error or loaded and usable; it must never panic the host.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::{capabilities, conversations};

fuzz_target!(|data: &[u8]| {
    let dir = std::env::temp_dir().join(format!("pbfuzz-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("state.json");
    if std::fs::write(&f, data).is_err() {
        return;
    }
    if let Ok(book) = conversations::Book::new(Some(f.clone())) {
        let _ = book.list("someone");
        let _ = book.read("someone", "c1", 0, false);
    }
    if let Ok(book) = capabilities::Book::new(vec![], Some(f)) {
        let _ = book.pending();
        let _ = book.view("someone", "id");
    }
});
