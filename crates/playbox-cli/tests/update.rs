//! `playbox update` end to end against a release directory served over file:// (no network): check, update, tamper, signature, rollback.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_playbox");

fn platform() -> String {
    let os = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    format!("{os}-{arch}")
}

fn sh(dir: &Path, script: &str) {
    assert!(Command::new("sh").arg("-c").arg(script).current_dir(dir).status().unwrap().success(), "{script}");
}

fn fresh(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("playbox-update-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A release directory holding version `v`: a fake `playbox` that reports it, `release.json` and `SHA256SUMS`.
fn release(root: &Path, v: &str) -> PathBuf {
    let rel = root.join("release");
    std::fs::create_dir_all(&rel).unwrap();
    let stage = root.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("playbox"), format!("#!/bin/sh\necho 'playbox {v} (fake)'\n")).unwrap();
    let asset = format!("playbox-{}.tar.gz", platform());
    sh(&stage, &format!("chmod 755 playbox && tar -czf {} playbox", rel.join(&asset).display()));
    std::fs::write(root.join("CHANGELOG.md"), format!("## [Unreleased]\n\n## [{v}] - 2026-01-01\n\n- the fake release\n")).unwrap();
    let out = Command::new(BIN)
        .args(["release", "manifest", v, rel.to_str().unwrap(), "--changelog", root.join("CHANGELOG.md").to_str().unwrap(), "--out", rel.join("release.json").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    sums(&rel);
    rel
}

fn sums(rel: &Path) {
    let mut lines = vec![];
    for n in [format!("playbox-{}.tar.gz", platform()), "release.json".into()] {
        let h: String = Sha256::digest(std::fs::read(rel.join(&n)).unwrap()).iter().map(|b| format!("{b:02x}")).collect();
        lines.push(format!("{h}  {n}\n"));
    }
    std::fs::write(rel.join("SHA256SUMS"), lines.concat()).unwrap();
}

/// An "installed" playbox: a copy of the real binary in its own directory, so the update may replace it.
fn install(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("playbox");
    std::fs::copy(BIN, &exe).unwrap();
    exe
}

fn run(exe: &Path, rel: &Path, args: &[&str]) -> Output {
    Command::new(exe).arg("update").args(args).env("PLAYBOX_BASE_URL", format!("file://{}", rel.display())).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn version_of(exe: &Path) -> String {
    String::from_utf8_lossy(&Command::new(exe).arg("--version").output().unwrap().stdout).trim().to_string()
}

#[test]
fn check_reports_and_exits_10_when_newer_and_0_when_current() {
    let root = fresh("check");
    let exe = install(&root);
    let rel = release(&root, "9.9.9");
    let o = run(&exe, &rel, &["--check"]);
    assert_eq!(o.status.code(), Some(10), "{}", text(&o));
    assert!(text(&o).contains("9.9.9") && text(&o).contains("the fake release"), "{}", text(&o));
    let j = run(&exe, &rel, &["--json"]);
    let v: serde_json::Value = serde_json::from_slice(&j.stdout).unwrap();
    assert_eq!(v["update_available"], true);
    assert_eq!(v["latest"], "9.9.9");
    assert_eq!(v["protocol"], 1);

    let same = release(&fresh("same"), env!("CARGO_PKG_VERSION"));
    let o = run(&exe, &same, &["--check"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("up to date"));
}

#[test]
fn update_swaps_the_binary_keeps_the_old_one_and_rollback_restores_it() {
    let root = fresh("swap");
    let exe = install(&root);
    let before = std::fs::read(&exe).unwrap();
    let rel = release(&root, "9.9.9");
    let o = run(&exe, &rel, &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(version_of(&exe).contains("9.9.9"), "{}", version_of(&exe));
    assert_eq!(std::fs::read(exe.with_file_name("playbox.prev")).unwrap(), before, "the old binary is kept");
    assert!(!exe.with_file_name(".playbox.new").exists(), "no staging file left behind");

    // Roll back with a real binary, since the swapped-in fake does not understand `update`.
    let root2 = fresh("rollback");
    let real = install(&root2);
    std::fs::write(real.with_file_name("playbox.prev"), "#!/bin/sh\necho 'playbox 0.0.1 (kept)'\n").unwrap();
    sh(&root2, "chmod 755 bin/playbox.prev");
    let o = Command::new(&real).args(["update", "--rollback"]).output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(version_of(&real).contains("0.0.1"));
}

#[test]
fn a_tampered_archive_or_manifest_is_refused_and_nothing_changes() {
    let root = fresh("tamper");
    let exe = install(&root);
    let before = std::fs::read(&exe).unwrap();
    let rel = release(&root, "9.9.9");
    let asset = rel.join(format!("playbox-{}.tar.gz", platform()));
    let mut b = std::fs::read(&asset).unwrap();
    b.push(0);
    std::fs::write(&asset, b).unwrap();
    let o = run(&exe, &rel, &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("checksum mismatch"), "{}", text(&o));
    assert_eq!(std::fs::read(&exe).unwrap(), before);

    let rel = release(&fresh("tamper2"), "9.9.9");
    let j = std::fs::read_to_string(rel.join("release.json")).unwrap().replace("the fake release", "something else");
    std::fs::write(rel.join("release.json"), j).unwrap();
    let o = run(&exe, &rel, &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("release.json does not match SHA256SUMS"), "{}", text(&o));
}

#[test]
fn a_signed_release_needs_the_matching_key() {
    let root = fresh("signed");
    let exe = install(&root);
    let rel = release(&root, "9.9.9");
    let keyfile = root.join("key");
    let k = Command::new(BIN).args(["release", "keygen", keyfile.to_str().unwrap()]).output().unwrap();
    let pubkey = String::from_utf8_lossy(&k.stdout).trim().to_string();
    let s = Command::new(BIN).args(["release", "sign", rel.join("SHA256SUMS").to_str().unwrap(), "--key", keyfile.to_str().unwrap()]).output().unwrap();
    assert!(s.status.success(), "{}", text(&s));

    let other = fresh("otherkey");
    let k2 = Command::new(BIN).args(["release", "keygen", other.join("k").to_str().unwrap()]).output().unwrap();
    let wrong = String::from_utf8_lossy(&k2.stdout).trim().to_string();
    let o = run(&exe, &rel, &["--pubkey", &wrong]);
    assert!(!o.status.success());
    assert!(text(&o).contains("BAD SIGNATURE"), "{}", text(&o));

    let o = run(&exe, &rel, &["--pubkey", &pubkey]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(version_of(&exe).contains("9.9.9"));

    // A key is pinned but the release carries no signature.
    let unsigned = release(&fresh("unsigned"), "9.9.9");
    let exe2 = install(&fresh("inst2"));
    let o = run(&exe2, &unsigned, &["--pubkey", &pubkey]);
    assert!(!o.status.success());
    assert!(text(&o).contains("not signed"), "{}", text(&o));
}

#[test]
fn it_never_updates_a_build_directory_binary() {
    let rel = release(&fresh("builddir"), "9.9.9");
    let o = Command::new(BIN).arg("update").env("PLAYBOX_BASE_URL", format!("file://{}", rel.display())).output().unwrap();
    assert!(!o.status.success());
    assert!(text(&o).contains("build directory"), "{}", text(&o));
}

#[test]
fn prepare_and_check_agree_on_a_fresh_workspace() {
    let root = fresh("prepare");
    std::fs::write(root.join("Cargo.toml"), "[workspace.package]\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(root.join("CHANGELOG.md"), "# Changelog\n\n## [Unreleased]\n\n### Added\n- one\n\n## [0.1.0] - 2026-01-01\n\n- first\n").unwrap();
    let c = root.join("CHANGELOG.md");
    let t = root.join("Cargo.toml");
    let (c, t) = (c.to_str().unwrap(), t.to_str().unwrap());
    let bad = Command::new(BIN).args(["release", "check", "0.2.0", "--changelog", c, "--cargo-toml", t]).output().unwrap();
    assert!(!bad.status.success(), "a tag before prepare must fail the gate");
    let p = Command::new(BIN).args(["release", "prepare", "0.2.0", "--changelog", c, "--cargo-toml", t]).current_dir(&root).output().unwrap();
    assert!(p.status.success(), "{}", text(&p));
    assert!(std::fs::read_to_string(t).unwrap().contains("version = \"0.2.0\""));
    let ok = Command::new(BIN).args(["release", "check", "0.2.0", "--changelog", c, "--cargo-toml", t]).output().unwrap();
    assert!(ok.status.success(), "{}", text(&ok));
    let n = Command::new(BIN).args(["release", "notes", "0.2.0", "--changelog", c]).output().unwrap();
    assert!(String::from_utf8_lossy(&n.stdout).contains("- one"));
    let again = Command::new(BIN).args(["release", "prepare", "0.1.5", "--changelog", c, "--cargo-toml", t]).current_dir(&root).output().unwrap();
    assert!(!again.status.success(), "going backwards is refused");
}
