//! Tests for prompt sets, the weights manifest, scratch directories and the output reader.

use super::files::{hex, read_output, weights_manifest, PromptSet, Scratch};

#[test]
fn prompt_sets_are_checked_like_the_engine_checks_them() {
    let ok = |s: &str| PromptSet::parse(s.as_bytes());
    assert_eq!(
        ok(r#"{"prompts":["a","b"],"max_new_tokens":10}"#).unwrap(),
        PromptSet {
            prompts: 2,
            max_new_tokens: 10
        }
    );
    assert_eq!(ok(r#"{"prompts":["a"]}"#).unwrap().max_new_tokens, 32);
    assert_eq!(
        ok(r#"{"prompts":["a","b"],"max_new_tokens":10}"#)
            .unwrap()
            .tokens(),
        40
    );
    for bad in [
        "[]",
        "{}",
        r#"{"prompts":[]}"#,
        r#"{"prompts":[""]}"#,
        r#"{"prompts":[1]}"#,
        r#"{"prompts":["a"],"max_new_tokens":0}"#,
        r#"{"prompts":["a"],"max_new_tokens":513}"#,
        r#"{"prompts":["a"],"max_new_tokens":1.5}"#,
        r#"{"prompts":["a"],"temperature":2.5}"#,
        r#"{"prompts":["a"],"temperature":"hot"}"#,
        r#"{"prompts":["a"],"extra":1}"#,
        "not json",
    ] {
        assert!(ok(bad).is_err(), "{bad}");
    }
    let many = format!(r#"{{"prompts":[{}]}}"#, vec!["\"a\""; 257].join(","));
    assert!(ok(&many).is_err());
}

#[test]
fn manifest_matches_the_python_engine() {
    // The expected value is `python -m playbox_steer manifest` over the same tree (see
    // steer/tests/test_engine.py::test_manifest_cross_check_fixture).
    let dir = std::env::temp_dir().join(format!("pb-manifest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "alpha").unwrap();
    std::fs::write(dir.join("a-b.txt"), "dash").unwrap();
    std::fs::write(dir.join("sub").join("c.bin"), [0u8, 1, 2, 3]).unwrap();
    std::fs::create_dir_all(dir.join(".cache")).unwrap();
    std::fs::write(dir.join(".cache").join("ignored"), "x").unwrap();
    std::os::unix::fs::symlink(dir.join("a.txt"), dir.join("link")).unwrap();
    assert_eq!(hex(&weights_manifest(&dir).unwrap()), MANIFEST_FIXTURE);
    std::fs::write(dir.join("a.txt"), "alphA").unwrap();
    assert_ne!(hex(&weights_manifest(&dir).unwrap()), MANIFEST_FIXTURE);
    let _ = std::fs::remove_dir_all(&dir);
}

// Filled in from the Python implementation; see test_manifest_cross_check_fixture.
const MANIFEST_FIXTURE: &str = "7bc07aaa2a32a98a50e6abae371e4e48a068edd147265ea594bee1c8f13ec519";

#[test]
fn scratch_dirs_are_removed_on_drop() {
    let p = {
        let s = Scratch::new().unwrap();
        std::fs::write(s.0.join("x"), "1").unwrap();
        s.0.clone()
    };
    assert!(!p.exists());
}

#[test]
fn output_reader_refuses_symlinks_directories_and_non_objects() {
    let dir = Scratch::new().unwrap();
    let f = dir.0.join("out.json");
    std::fs::write(&f, r#"{"a":1}"#).unwrap();
    assert!(read_output(&f).is_ok());
    std::fs::write(&f, "[1]").unwrap();
    assert!(read_output(&f).is_err());
    std::fs::write(&f, "garbage").unwrap();
    assert!(read_output(&f).is_err());
    let target = dir.0.join("secret");
    std::fs::write(&target, r#"{"secret":true}"#).unwrap();
    let link = dir.0.join("link.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(read_output(&link).is_err(), "symlink must not be followed");
    assert!(read_output(&dir.0.join("missing.json")).is_err());
}
