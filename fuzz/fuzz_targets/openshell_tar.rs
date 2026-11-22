#![no_main]
use libfuzzer_sys::fuzz_target;

// Fuzz OpenShell's untrusted sandbox tar validator and path/symlink validators.
// Must never panic, overflow, or allow unsafe paths/symlinks through.
fuzz_target!(|data: &[u8]| {
    if let Ok(entries) = playbox_runner::validate_openshell_tar_bytes(data) {
        for e in &entries {
            assert!(!e.is_empty());
            assert!(!e.starts_with('/'));
            assert!(playbox_runner::validate_openshell_rel_path(e).is_ok());
        }
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = playbox_runner::validate_openshell_rel_path(s);
        if let Some((entry, target)) = s.split_once('\0') {
            let _ = playbox_runner::validate_openshell_symlink_target(entry, target);
        } else if let Some((entry, target)) = s.split_once("->") {
            let _ = playbox_runner::validate_openshell_symlink_target(entry, target);
        }
    }
});
