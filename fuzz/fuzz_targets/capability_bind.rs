//! A capability definition is owner-written JSON and its arguments are a member's text. Whatever either is, validation and binding
//! must refuse cleanly or build a command whose program is the fixed first entry and whose values carry no control characters.
#![no_main]
use libfuzzer_sys::fuzz_target;
use playbox_net::capabilities::Capability;
use std::collections::BTreeMap;

fuzz_target!(|data: &[u8]| {
    // The first line is the definition; the rest is split into name=value argument lines.
    let (def, rest) = match data.iter().position(|b| *b == b'\n') {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[][..]),
    };
    let Ok(cap) = serde_json::from_slice::<Capability>(def) else { return };
    if cap.validate().is_err() {
        return;
    }
    let mut args = BTreeMap::new();
    for line in String::from_utf8_lossy(rest).lines().take(40) {
        if let Some((k, v)) = line.split_once('=') {
            args.insert(k.to_string(), v.to_string());
        }
    }
    if let Ok(argv) = cap.bind(&args) {
        assert_eq!(argv.len(), cap.argv.len());
        assert_eq!(argv[0], cap.argv[0], "a member's text must never change the program");
        for v in args.values() {
            assert!(!v.chars().any(char::is_control), "a control character got through");
        }
    }
});
