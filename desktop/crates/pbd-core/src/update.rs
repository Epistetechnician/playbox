//! Finding and fetching a newer desktop build, with exactly the trust `playbox update` uses (docs/releasing.md):
//!
//! 1. `SHA256SUMS` is verified against `SHA256SUMS.sig` with an ed25519 key pinned in this binary (`release.pub`).
//! 2. `release.json` and the desktop archive must match the hashes in that signed list.
//! 3. The caller then checks that the unpacked app reports the version `release.json` promised, before swapping anything.
//!
//! Nothing here replaces files or restarts anything: the app decides what to do with a verified archive. A release without a
//! desktop archive (or an unsigned one) is simply "no update". Network access goes through `curl`, as the CLI's updater does.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "Epistetechnician/playbox";

/// The release signing keys this build trusts (hex ed25519, one per line): `release.pub`, or `PLAYBOX_RELEASE_PUBKEY` at build time.
const PINNED_KEYS: &str = match option_env!("PLAYBOX_RELEASE_PUBKEY") {
    Some(k) if !k.is_empty() => k,
    _ => include_str!("../../../../release.pub"),
};

pub fn pinned_keys() -> Vec<String> {
    PINNED_KEYS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Stable versions only (`1.2.3`): a pre-release string is not a version we install.
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.trim().trim_start_matches('v').split('.');
        let v = Version(
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        );
        it.next().is_none().then_some(v)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Fetch(String),
    /// The checksum list is not signed by any pinned key (or has no signature).
    Signature,
    NoKeys,
    Manifest(String),
    Hash {
        name: String,
    },
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fetch(e) => write!(f, "could not download: {e}"),
            Error::Signature => write!(
                f,
                "the release is not signed by a key this app trusts; not updating"
            ),
            Error::NoKeys => write!(f, "this build pins no release key; not updating"),
            Error::Manifest(e) => write!(f, "release.json: {e}"),
            Error::Hash { name } => write!(f, "{name} does not match the signed checksum list"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where releases come from. `PLAYBOX_BASE_URL` (the CLI's mirror variable) may point at plain http or a `file://` directory.
#[derive(Clone, Debug)]
pub struct Source {
    base: String,
    relaxed: bool,
}

impl Source {
    pub fn github_latest() -> Self {
        match std::env::var("PLAYBOX_BASE_URL") {
            Ok(b) if !b.is_empty() => Self::mirror(&b),
            _ => Source {
                base: format!("https://github.com/{REPO}/releases/latest/download"),
                relaxed: false,
            },
        }
    }

    pub fn mirror(base: &str) -> Self {
        Source {
            base: base.trim_end_matches('/').to_string(),
            relaxed: true,
        }
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }

    fn curl(&self, name: &str, out: Option<&Path>) -> Result<Vec<u8>, Error> {
        let url = self.url(name);
        let args = curl_args(&url, self.relaxed, out)?;
        let o = Command::new("curl")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::Fetch(format!("running curl: {e}")))?;
        if !o.status.success() {
            return Err(Error::Fetch(
                String::from_utf8_lossy(&o.stderr).trim().to_string(),
            ));
        }
        Ok(o.stdout)
    }
}

fn curl_args(
    url: &str,
    relaxed: bool,
    out: Option<&Path>,
) -> Result<Vec<std::ffi::OsString>, Error> {
    if url.is_empty() || url.starts_with('-') || url.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::Fetch(format!("refusing unsafe update URL: {url:?}")));
    }
    let proto = if relaxed {
        "=https,http,file"
    } else {
        "=https"
    };
    let mut args: Vec<std::ffi::OsString> = [
        "--proto",
        proto,
        "--proto-redir",
        proto,
        "--tlsv1.2",
        "-fsSL",
        "--retry",
        "3",
        "--max-time",
        "600",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect();
    if let Some(p) = out {
        args.push(std::ffi::OsString::from("-o"));
        args.push(p.as_os_str().to_os_string());
    }
    args.push(std::ffi::OsString::from("--"));
    args.push(std::ffi::OsString::from(url));
    Ok(args)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// `hash  name` lines as `sha256sum` writes them.
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (h, n) = l.split_once(char::is_whitespace)?;
            Some((n.trim().trim_start_matches('*').to_string(), h.to_string()))
        })
        .collect()
}

/// Is `sig_hex` an ed25519 signature of `message` by any of `keys`?
pub fn signed_by_any(message: &[u8], sig_hex: &str, keys: &[String]) -> bool {
    let Some(sig) = unhex::<64>(sig_hex).map(|b| Signature::from_bytes(&b)) else {
        return false;
    };
    keys.iter().any(|k| {
        unhex::<32>(k)
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .is_some_and(|vk| vk.verify(message, &sig).is_ok())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub asset: String,
    pub sha256: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    /// A newer release exists but carries no build for this platform.
    NoBuild,
    Available(Release),
}

/// Look at the latest release: verified, never trusting anything the signature does not cover.
pub fn check(src: &Source, current: Version, asset: &str, keys: &[String]) -> Result<Check, Error> {
    if keys.is_empty() {
        return Err(Error::NoKeys);
    }
    let sums_raw = src.curl("SHA256SUMS", None)?;
    let sig = src
        .curl("SHA256SUMS.sig", None)
        .map_err(|_| Error::Signature)?;
    if !signed_by_any(&sums_raw, &String::from_utf8_lossy(&sig), keys) {
        return Err(Error::Signature);
    }
    let sums = parse_sums(&String::from_utf8_lossy(&sums_raw));

    let manifest_hash = sums
        .get("release.json")
        .ok_or_else(|| Error::Manifest("not in the checksum list".into()))?;
    let manifest = src.curl("release.json", None)?;
    if &sha256_hex(&manifest) != manifest_hash {
        return Err(Error::Hash {
            name: "release.json".into(),
        });
    }
    let v: serde_json::Value =
        serde_json::from_slice(&manifest).map_err(|e| Error::Manifest(e.to_string()))?;
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .and_then(Version::parse)
        .ok_or_else(|| Error::Manifest("no stable version".into()))?;
    if version <= current {
        return Ok(Check::UpToDate);
    }
    Ok(match sums.get(asset) {
        Some(h) => Check::Available(Release {
            version,
            asset: asset.to_string(),
            sha256: h.clone(),
        }),
        None => Check::NoBuild,
    })
}

/// Download the release's archive into `dir` and check it against the signed hash. On a mismatch the file is deleted.
pub fn download(src: &Source, rel: &Release, dir: &Path) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(e.to_string()))?;
    let dest = dir.join(&rel.asset);
    src.curl(&rel.asset, Some(&dest))?;
    let got = sha256_file(&dest).map_err(|e| Error::Io(e.to_string()))?;
    if got != rel.sha256 {
        let _ = std::fs::remove_file(&dest);
        return Err(Error::Hash {
            name: rel.asset.clone(),
        });
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const ASSET: &str = "Playbox-macos-arm64.zip";

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pubhex(k: &SigningKey) -> String {
        hex(&k.verifying_key().to_bytes())
    }

    /// A release directory the way the workflow publishes it.
    struct Rel {
        dir: PathBuf,
    }

    fn release(version: &str, with_asset: bool, signer: &SigningKey, tamper: &str) -> Rel {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pbd-update-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = b"pretend zip";
        let manifest = format!(r#"{{"version":"{version}","assets":{{}}}}"#);
        let mut sums = format!("{}  release.json\n", sha256_hex(manifest.as_bytes()));
        if with_asset {
            sums += &format!("{}  {ASSET}\n", sha256_hex(archive));
            std::fs::write(dir.join(ASSET), archive).unwrap();
        }
        let sig = hex(&signer.sign(sums.as_bytes()).to_bytes());
        std::fs::write(dir.join("release.json"), &manifest).unwrap();
        std::fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        std::fs::write(dir.join("SHA256SUMS.sig"), format!("{sig}\n")).unwrap();
        match tamper {
            "manifest" => {
                std::fs::write(dir.join("release.json"), manifest.replace('0', "9")).unwrap()
            }
            "archive" => std::fs::write(dir.join(ASSET), b"evil").unwrap(),
            _ => {}
        }
        Rel { dir }
    }

    impl Rel {
        fn src(&self) -> Source {
            Source::mirror(&format!("file://{}", self.dir.display()))
        }
    }
    impl Drop for Rel {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_order_and_reject_prereleases() {
        assert!(
            v("0.3.15") > v("0.3.14") && v("0.10.0") > v("0.9.9") && v("v1.0.0") > v("0.99.99")
        );
        assert_eq!(Version::parse("0.3.0-rc.1"), None);
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
    }

    #[test]
    fn the_repos_pinned_key_file_has_a_usable_key() {
        let keys = pinned_keys();
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|k| unhex::<32>(k).is_some()));
    }

    #[test]
    fn a_newer_signed_release_is_offered_and_downloads_verified() {
        let k = key(1);
        let r = release("0.3.15", true, &k, "");
        let c = check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]).unwrap();
        let Check::Available(rel) = c else {
            panic!("expected an update")
        };
        assert_eq!(rel.version, v("0.3.15"));
        let out = download(&r.src(), &rel, &r.dir.join("dl")).unwrap();
        assert_eq!(std::fs::read(out).unwrap(), b"pretend zip");
    }

    #[test]
    fn same_or_older_is_up_to_date() {
        let k = key(1);
        let r = release("0.3.14", true, &k, "");
        assert_eq!(
            check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]).unwrap(),
            Check::UpToDate
        );
        assert_eq!(
            check(&r.src(), v("0.4.0"), ASSET, &[pubhex(&k)]).unwrap(),
            Check::UpToDate
        );
    }

    #[test]
    fn a_release_signed_by_another_key_is_refused() {
        let r = release("0.3.15", true, &key(2), "");
        assert_eq!(
            check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&key(1))]),
            Err(Error::Signature)
        );
    }

    #[test]
    fn any_pinned_key_is_enough_so_keys_can_rotate() {
        let r = release("0.3.15", true, &key(2), "");
        let keys = [pubhex(&key(1)), pubhex(&key(2))];
        assert!(matches!(
            check(&r.src(), v("0.3.14"), ASSET, &keys),
            Ok(Check::Available(_))
        ));
    }

    #[test]
    fn a_missing_signature_is_refused() {
        let k = key(1);
        let r = release("0.3.15", true, &k, "");
        std::fs::remove_file(r.dir.join("SHA256SUMS.sig")).unwrap();
        assert_eq!(
            check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]),
            Err(Error::Signature)
        );
    }

    #[test]
    fn no_pinned_key_means_no_update() {
        let r = release("0.3.15", true, &key(1), "");
        assert_eq!(check(&r.src(), v("0.3.14"), ASSET, &[]), Err(Error::NoKeys));
    }

    #[test]
    fn a_tampered_manifest_or_archive_is_refused() {
        let k = key(1);
        let r = release("0.3.15", true, &k, "manifest");
        assert!(matches!(
            check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]),
            Err(Error::Hash { .. }) | Err(Error::Manifest(_))
        ));
        let r = release("0.3.15", true, &k, "archive");
        let Ok(Check::Available(rel)) = check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]) else {
            panic!()
        };
        assert!(matches!(
            download(&r.src(), &rel, &r.dir.join("dl")),
            Err(Error::Hash { .. })
        ));
        assert!(
            !r.dir.join("dl").join(ASSET).exists(),
            "a bad archive is deleted"
        );
    }

    #[test]
    fn a_release_without_a_desktop_build_is_no_build() {
        let k = key(1);
        let r = release("0.3.15", false, &k, "");
        assert_eq!(
            check(&r.src(), v("0.3.14"), ASSET, &[pubhex(&k)]).unwrap(),
            Check::NoBuild
        );
    }

    #[test]
    fn curl_args_enforces_proto_redir_and_blocks_flag_injection() {
        let args = curl_args("https://example.com/SHA256SUMS", false, None).unwrap();
        let strs: Vec<_> = args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let redir_pos = strs.iter().position(|a| a == "--proto-redir").unwrap();
        assert_eq!(strs[redir_pos + 1], "=https");
        let sep_pos = strs.iter().position(|a| a == "--").unwrap();
        assert_eq!(strs[sep_pos + 1], "https://example.com/SHA256SUMS");

        assert!(curl_args("--config=/tmp/evil", false, None).is_err());
        assert!(curl_args("https://example.com/\n--next", false, None).is_err());
        assert!(curl_args("", false, None).is_err());
    }
}
