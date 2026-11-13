//! `playbox update`: replace the running binary with a newer release, after checking it.
//!
//! A release publishes `release.json` (version, wire protocol, per-asset SHA-256, notes), `SHA256SUMS` and, when the maintainer has a
//! key, `SHA256SUMS.sig`. The update reads the manifest, downloads this platform's archive, checks its hash against `SHA256SUMS`, checks
//! that file's signature against the key compiled into this binary, runs the new binary's `--version` as a smoke test, then swaps it
//! in with one `rename` (atomic on the same filesystem) and keeps the old binary beside it for `--rollback`.
//!
//! Downloads go through `curl` and archives through `tar`, the same tools `install.sh` needs, so the CLI carries no HTTP stack.
//! `PLAYBOX_BASE_URL` points it at a mirror, or at a `file://` directory in tests.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "Epistetechnician/playbox";
/// The release signing keys (ed25519, hex) this build trusts: `release.pub` at the repository root, or `PLAYBOX_RELEASE_PUBKEY` at
/// build time to replace it (CI, a fork). More than one so a key can be rotated without stranding old installs.
const PINNED_KEYS: &str = match option_env!("PLAYBOX_RELEASE_PUBKEY") {
    Some(k) if !k.is_empty() => k,
    _ => include_str!("../../../release.pub"),
};

fn pinned_keys() -> Vec<String> {
    PINNED_KEYS.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}
/// Exit status of `--check` when a newer release exists, so scripts can branch on it.
pub const EXIT_UPDATE_AVAILABLE: u8 = 10;

#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub check: bool,
    pub version: Option<String>,
    pub rollback: bool,
    pub force: bool,
    pub json: bool,
    pub pubkey: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub protocol: Option<u32>,
    #[serde(default)]
    pub released: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub assets: BTreeMap<String, String>,
}

/// A parsed `MAJOR.MINOR.PATCH[-pre]`. A pre-release sorts below its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    nums: [u64; 3],
    pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().trim_start_matches('v');
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => bail!("{s:?} is not a version"),
            None => (s, None),
        };
        let mut it = core.split('.');
        let mut nums = [0u64; 3];
        for n in &mut nums {
            *n = it.next().ok_or_else(|| anyhow!("{s:?} is not MAJOR.MINOR.PATCH"))?.parse().map_err(|_| anyhow!("{s:?} is not MAJOR.MINOR.PATCH"))?;
        }
        if it.next().is_some() {
            bail!("{s:?} is not MAJOR.MINOR.PATCH");
        }
        Ok(Self { nums, pre })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.nums[0], self.nums[1], self.nums[2])?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.nums.cmp(&o.nums).then_with(|| match (&self.pre, &o.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        })
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is valid")
}

/// `os-arch` as the release names its archives.
fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        o => bail!("no prebuilt release for {o}; build from source: cargo install --path crates/playbox-cli"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        a => bail!("no prebuilt release for {a}"),
    };
    Ok(format!("{os}-{arch}"))
}

fn base_url(version: Option<&str>) -> String {
    if let Ok(b) = std::env::var("PLAYBOX_BASE_URL") {
        return b.trim_end_matches('/').to_string();
    }
    match version {
        Some(v) => format!("https://github.com/{REPO}/releases/download/v{}", v.trim_start_matches('v')),
        None => format!("https://github.com/{REPO}/releases/latest/download"),
    }
}

fn fetch(url: &str) -> Result<Vec<u8>> {
    // A mirror set through the environment may be plain http or a local directory; the default is https only.
    let proto = if std::env::var_os("PLAYBOX_BASE_URL").is_some() { "=https,http,file" } else { "=https" };
    let out = Command::new("curl")
        .args(["--proto", proto, "--tlsv1.2", "-fsSL", "--retry", "3", "--max-time", "300", url])
        .stdin(Stdio::null())
        .output()
        .context("running curl (the updater needs curl, as install.sh does)")?;
    if !out.status.success() {
        bail!("could not download {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().fold(String::new(), |mut s, x| {
        use std::fmt::Write;
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// `hash  name` lines as `sha256sum` writes them (a `*` before the name marks binary mode).
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (h, n) = l.split_once(char::is_whitespace)?;
            Some((n.trim().trim_start_matches('*').to_string(), h.to_string()))
        })
        .collect()
}

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Result<Self> {
        let d = std::env::temp_dir().join(format!("playbox-update-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&d)?;
        Ok(Self(d))
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn manifest(base: &str) -> Result<Manifest> {
    let raw = fetch(&format!("{base}/release.json"))
        .context("this release has no release.json (releases before 0.2 do not); reinstall with install.sh to move onto one that does")?;
    serde_json::from_slice(&raw).context("release.json is malformed")
}

/// The exe to replace, refusing locations an update should not touch.
fn target_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let s = exe.to_string_lossy();
    if s.contains("/target/debug/") || s.contains("/target/release/") {
        bail!("{} is a build directory; update by rebuilding (git pull, cargo build --release)", exe.display());
    }
    if s.contains("/Cellar/") || s.contains("/nix/store/") {
        bail!("{} is managed by a package manager; update it with that tool", exe.display());
    }
    let dir = exe.parent().ok_or_else(|| anyhow!("no directory for {}", exe.display()))?;
    let probe = dir.join(format!(".playbox-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| anyhow!("cannot write to {}: {e}. Reinstall with install.sh into a directory you own", dir.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(exe)
}

fn prev_path(exe: &Path) -> PathBuf {
    exe.with_file_name("playbox.prev")
}

pub fn run(o: &Opts) -> Result<u8> {
    if o.rollback {
        return rollback();
    }
    let want = o.version.as_deref().map(Version::parse).transpose()?;
    let base = base_url(want.as_ref().map(ToString::to_string).as_deref());
    let m = manifest(&base)?;
    let latest = Version::parse(&m.version)?;
    let cur = current();
    let target = want.clone().unwrap_or(latest.clone());
    if want.is_some() && target.to_string() != Version::parse(&m.version)?.to_string() {
        bail!("{base} serves {}, not {target}", m.version);
    }
    let newer = target > cur;
    let compat = match m.protocol {
        Some(p) if p > playbox_net::wire::PROTOCOL => format!("changes the wire protocol ({} -> {p}): rooms hosted by older builds will refuse you until their hosts update", playbox_net::wire::PROTOCOL),
        Some(p) => format!("wire protocol {p}: compatible with rooms on this version"),
        None => "wire protocol not stated".to_string(),
    };
    if o.json {
        println!(
            "{}",
            serde_json::json!({"current": cur.to_string(), "latest": m.version, "update_available": newer, "protocol": m.protocol, "released": m.released, "notes": m.notes})
        );
    } else if o.check || !newer && !o.force {
        if newer {
            println!("playbox {cur} -> {target} available ({})", m.released);
            println!("{compat}");
            if !m.notes.trim().is_empty() {
                println!("\n{}", m.notes.trim());
            }
            println!("\nrun: playbox update");
        } else {
            println!("playbox {cur} is up to date");
        }
    }
    if o.check || o.json {
        return Ok(if newer { EXIT_UPDATE_AVAILABLE } else { 0 });
    }
    if !newer && !o.force && want.is_none() {
        return Ok(0);
    }
    if target < cur && want.is_none() {
        return Ok(0);
    }

    let exe = target_exe()?;
    let asset = format!("playbox-{}.tar.gz", platform()?);
    let tmp = TempDir::new()?;

    // 1. The checksum list, and its signature against the key this build trusts.
    let sums_raw = fetch(&format!("{base}/SHA256SUMS"))?;
    let sums_path = tmp.0.join("SHA256SUMS");
    std::fs::write(&sums_path, &sums_raw)?;
    let keys = o.pubkey.clone().map_or_else(pinned_keys, |k| vec![k]);
    if keys.is_empty() {
        eprintln!("note: this build pins no release key, so the update is checked against SHA256SUMS only");
    } else {
        let sig = fetch(&format!("{base}/SHA256SUMS.sig")).context("this release is not signed, and this build requires a signature")?;
        std::fs::write(tmp.0.join("SHA256SUMS.sig"), sig)?;
        let ok = keys.iter().any(|k| crate::release::verify_quiet(&sums_path.to_string_lossy(), None, k).is_ok());
        if !ok {
            bail!("BAD SIGNATURE: SHA256SUMS was not signed by any key this build trusts ({} pinned); not updating", keys.len());
        }
    }
    let sums = parse_sums(&String::from_utf8_lossy(&sums_raw));

    // 2. The manifest is covered by the same list, and the archive by both.
    if let Some(h) = sums.get("release.json") {
        let again = fetch(&format!("{base}/release.json"))?;
        if &sha256_hex(&again) != h {
            bail!("release.json does not match SHA256SUMS; not updating");
        }
    }
    let want_hash = sums.get(&asset).ok_or_else(|| anyhow!("{asset} is not in SHA256SUMS (no prebuilt release for this platform?)"))?;
    if let Some(mh) = m.assets.get(&asset) {
        if mh != want_hash {
            bail!("release.json and SHA256SUMS disagree about {asset}; not updating");
        }
    }
    eprintln!("downloading {asset} ({target})");
    let archive = fetch(&format!("{base}/{asset}"))?;
    let got = sha256_hex(&archive);
    if &got != want_hash {
        bail!("checksum mismatch for {asset} (expected {want_hash}, got {got}); not updating");
    }

    // 3. Unpack, smoke-test, swap.
    let ar = tmp.0.join(&asset);
    std::fs::write(&ar, &archive)?;
    let st = Command::new("tar").arg("-xzf").arg(&ar).arg("-C").arg(&tmp.0).status().context("running tar")?;
    if !st.success() {
        bail!("could not unpack {asset}");
    }
    let new_bin = tmp.0.join("playbox");
    if !new_bin.is_file() {
        bail!("the archive has no playbox binary");
    }
    #[cfg(unix)]
    std::fs::set_permissions(&new_bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    let ver = Command::new(&new_bin).arg("--version").stdin(Stdio::null()).output().context("running the downloaded binary")?;
    let ver_text = String::from_utf8_lossy(&ver.stdout);
    if !ver.status.success() || !ver_text.contains(&target.to_string()) {
        bail!("the downloaded binary does not report version {target} (it said {:?}); not updating", ver_text.trim());
    }
    let staged = exe.with_file_name(".playbox.new");
    std::fs::copy(&new_bin, &staged).with_context(|| format!("writing {}", staged.display()))?;
    #[cfg(unix)]
    std::fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::copy(&exe, prev_path(&exe)).context("keeping the old binary for --rollback")?;
    std::fs::rename(&staged, &exe).context("replacing the binary")?;

    println!("updated playbox {cur} -> {target}");
    println!("{compat}");
    if !m.notes.trim().is_empty() {
        println!("\n{}", m.notes.trim());
    }
    println!("\nrunning agents keep the old version until restarted; undo with: playbox update --rollback");
    Ok(0)
}

fn rollback() -> Result<u8> {
    let exe = target_exe()?;
    let prev = prev_path(&exe);
    if !prev.is_file() {
        bail!("no previous version kept at {}", prev.display());
    }
    let ver = Command::new(&prev).arg("--version").stdin(Stdio::null()).output().context("running the kept binary")?;
    if !ver.status.success() {
        bail!("the kept binary does not run; not rolling back");
    }
    let staged = exe.with_file_name(".playbox.new");
    std::fs::copy(&prev, &staged)?;
    std::fs::copy(&exe, &prev)?; // so a second --rollback undoes this one
    std::fs::rename(&staged, &exe)?;
    println!("rolled back to {}", String::from_utf8_lossy(&ver.stdout).trim());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_the_way_semver_does() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("0.10.0") > v("0.9.0"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        for bad in ["", "1", "1.2", "1.2.3.4", "a.b.c", "1.2.3-"] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(v("1.2.3-beta.1").to_string(), "1.2.3-beta.1");
    }

    #[test]
    fn sums_parse_both_sha256sum_modes() {
        let m = parse_sums("aa  playbox-linux-x86_64.tar.gz\nbb *release.json\n\n");
        assert_eq!(m["playbox-linux-x86_64.tar.gz"], "aa");
        assert_eq!(m["release.json"], "bb");
    }

    #[test]
    fn the_hash_matches_a_known_value() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
