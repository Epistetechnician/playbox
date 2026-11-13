//! The fuzzer has to be able to fail. These tests run `playbox secure` against the real hub code, then against copies of it with one
//! deliberate hole each, and check that every hole is reported and that the real code is clean.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Run the fuzzer against `<hub_root>/ui` with a private home; returns the `fuzz` object of the JSON report.
fn fuzz(hub_root: &Path, tag: &str) -> Value {
    let home = std::env::temp_dir().join(format!("pbsec-it-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_playbox"))
        .args(["secure", "--json", "--seed", "5", "--port", "1"]) // port 1: nothing live to probe
        .arg("--hub-dir")
        .arg(hub_root)
        .env("HOME", &home)
        .env("PLAYBOX_HOME", home.join(".playbox"))
        .env("PLAYBOX_HUB_HOME", home.join("hubstate"))
        .env("PLAYBOX_SERVICE_LABEL", "com.playbox.sectest")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let text = String::from_utf8_lossy(&o.stdout);
    let line = text.lines().rev().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no JSON in: {text}\n{}", String::from_utf8_lossy(&o.stderr)));
    serde_json::from_str::<Value>(line).unwrap()["fuzz"].clone()
}

fn kinds(f: &Value) -> Vec<String> {
    f["violations"].as_array().unwrap().iter().map(|v| v["kind"].as_str().unwrap().to_string()).collect()
}

/// A copy of `ui/` with `from` replaced by `to` in `file`.
fn mutant(tag: &str, file: &str, from: &str, to: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("pbsec-mut-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    assert!(Command::new("cp").arg("-R").arg(repo().join("ui")).arg(root.join("ui")).status().unwrap().success());
    let p = root.join("ui").join(file);
    let src = std::fs::read_to_string(&p).unwrap();
    assert!(src.contains(from), "the hub changed: {from:?} is gone from {file}; update this test");
    std::fs::write(&p, src.replacen(from, to, 1)).unwrap();
    root
}

#[test]
fn the_real_hub_survives_the_fuzzer() {
    let f = fuzz(&repo(), "real");
    assert!(f["violations"].is_array(), "the fuzzer produced no result (did the hub start?): {f}");
    assert!(f["violations"].as_array().unwrap().is_empty(), "{f}");
    assert_eq!(f["signed_in"], true, "the fuzzer must get past the front door to test what is behind it");
    assert!(f["requests"].as_u64().unwrap() > 1000);
}

#[test]
fn a_hub_that_lets_bad_json_crash_a_handler_is_caught() {
    let root = mutant("json", "hub/server.py", "            except ValueError:\n                raise AuthError(400, \"bad json\")", "            except ValueError:\n                raise");
    let k = kinds(&fuzz(&root, "json"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(k.iter().any(|k| k.contains("dropped") || k.contains("unhandled") || k.contains("server error")), "{k:?}");
}

#[test]
fn a_hub_with_an_open_private_route_is_caught() {
    let root = mutant("open", "hub/server.py", "            # public shell\n", "            if p == \"/api/me\":\n                return self.j(200, {\"leak\": 1})\n            # public shell\n");
    let k = kinds(&fuzz(&root, "open"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(k.iter().any(|k| k.contains("isn't signed in")), "{k:?}");
}

#[test]
fn a_hub_that_opens_cors_is_caught() {
    let root = mutant("cors", "hub/server.py", "            self.send_header(\"X-Frame-Options\", \"DENY\")\n", "            self.send_header(\"X-Frame-Options\", \"DENY\")\n            self.send_header(\"Access-Control-Allow-Origin\", \"*\")\n");
    let k = kinds(&fuzz(&root, "cors"));
    let _ = std::fs::remove_dir_all(&root);
    assert!(k.iter().any(|k| k.contains("CORS")), "{k:?}");
}
