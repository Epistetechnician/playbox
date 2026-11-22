//! Native owner step-up bridge for Playbox Desktop (`desktop-stepup/`).
//!
//! Embedded webviews (`WKWebView`, `WebKitGTK`) cannot run platform passkeys on `localhost`. When a `local_only` desktop session
//! attempts a sensitive action (revoking a member, delegating a task, adopting a local room, approving a capability call, pairing or
//! removing a device), the hub writes an owner-only challenge `<id>.req` under `<hub_home>/desktop-stepup/`.
//!
//! This watcher proves both filesystem ownership of `<hub_home>` AND live human presence via the operating system's own owner
//! verification (Touch ID / Apple Watch / user password on macOS, Polkit on Linux, Windows Hello / consent on Windows), then writes
//! `<id>.res` carrying `sha256("playbox-stepup-v1:" + nonce)`. No Tauri IPC is exposed to remote pages and no `unsafe` code is used.

use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DIR: &str = "desktop-stepup";
const READY_FILE: &str = "ready";
const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
const POLL_EVERY: Duration = Duration::from_millis(150);

/// Ask the human at the screen to approve a sensitive action. Separated as a trait so every branch of the protocol is tested headless.
pub trait Approver: Send + Sync + 'static {
    fn available(&self) -> bool {
        true
    }
    fn approve(&self, reason: &str) -> Result<bool, String>;
}

/// Operating-system biometric / owner verification.
pub struct OsApprover;

impl Approver for OsApprover {
    fn available(&self) -> bool {
        os_available()
    }
    fn approve(&self, reason: &str) -> Result<bool, String> {
        os_approve(sanitize_reason(reason))
    }
}

#[cfg(target_os = "macos")]
fn os_available() -> bool {
    Path::new("/usr/bin/osascript").is_file()
}

#[cfg(target_os = "linux")]
fn os_available() -> bool {
    Path::new("/usr/bin/pkexec").is_file()
        || std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|d| d.join("pkexec").is_file()))
            .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn os_available() -> bool {
    true
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_available() -> bool {
    false
}

fn is_bidi_or_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn sanitize_reason(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !is_bidi_or_format(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect();
    if cleaned.is_empty() {
        "approve this action in Playbox".to_string()
    } else {
        cleaned
    }
}

#[cfg(target_os = "macos")]
fn os_approve(reason: String) -> Result<bool, String> {
    // `LAPolicyDeviceOwnerAuthentication` (2) prompts Touch ID / Apple Watch first and falls back to the macOS login password.
    // `reason` travels exclusively in `PB_REASON` so untrusted text can never alter the script.
    const JXA: &str = r#"
ObjC.import("Foundation");
ObjC.import("LocalAuthentication");
var env = $.NSProcessInfo.processInfo.environment;
var raw = env.objectForKey("PB_REASON");
var reason = (raw && !raw.isNil()) ? ObjC.unwrap(raw) : "approve this action in Playbox";
var ctx = $.LAContext.alloc.init;
var err = Ref();
if (!ctx.canEvaluatePolicyError(2, err)) {
  "unsupported";
} else {
  var done = false, ok = false;
  ctx.evaluatePolicyLocalizedReasonReply(2, "Playbox: " + reason, function(res, _e) {
    ok = !!res;
    done = true;
  });
  var loop = $.NSRunLoop.currentRunLoop;
  var deadline = $.NSDate.dateWithTimeIntervalSinceNow(55.0);
  while (!done && loop.runModeBeforeDate($.NSDefaultRunLoopMode, $.NSDate.dateWithTimeIntervalSinceNow(0.05))) {
    if ($.NSDate.date.compare(deadline) >= 0) break;
  }
  ok ? "ok" : "cancelled";
}
"#;
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", JXA])
        .env("PB_REASON", reason)
        .output()
        .map_err(|e| format!("could not launch Touch ID / macOS authentication: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let token = stdout.trim();
    if out.status.success() && token == "ok" {
        Ok(true)
    } else if token == "unsupported" {
        Err("macOS owner authentication is not available on this session".into())
    } else {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
fn os_approve(_reason: String) -> Result<bool, String> {
    let status = std::process::Command::new("pkexec")
        .args(["--disable-internal-agent", "/usr/bin/true"])
        .status()
        .map_err(|e| format!("Polkit (pkexec) is unavailable: {e}"))?;
    Ok(status.success())
}

#[cfg(target_os = "windows")]
fn os_approve(reason: String) -> Result<bool, String> {
    const PS: &str = r#"
$reason = if ($env:PB_REASON) { $env:PB_REASON } else { 'Approve this action in Playbox' }
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
$op = [Windows.Security.Credentials.UI.UserConsentVerifier,Windows.Security.Credentials.UI,ContentType=WindowsRuntime]::RequestVerificationAsync("Playbox: $reason")
$netTask = $asTask.MakeGenericMethod([Windows.Security.Credentials.UI.UserConsentVerificationResult]).Invoke($null, @($op))
$res = $netTask.GetAwaiter().GetResult()
if ($res -eq [Windows.Security.Credentials.UI.UserConsentVerificationResult]::Verified) { Write-Output 'ok' } else { Write-Output 'cancelled' }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", PS])
        .env("PB_REASON", reason)
        .output()
        .map_err(|e| format!("Windows Hello / owner verification failed: {e}"))?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok")
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_approve(_reason: String) -> Result<bool, String> {
    Err("owner verification is not supported on this operating system".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn compute_proof(nonce: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"playbox-stepup-v1:");
    h.update(nonce.as_bytes());
    hex(&h.finalize())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other("step-up path is not a directory"));
    }
    let me = rustix::process::getuid().as_raw();
    if meta.uid() != me {
        return Err(io::Error::other("step-up directory owned by another user"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn is_safe_private_file(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let me = rustix::process::getuid().as_raw();
    meta.file_type().is_file() && meta.uid() == me && (meta.permissions().mode() & 0o077) == 0
}

#[cfg(not(unix))]
fn is_safe_private_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

#[cfg(unix)]
fn is_safe_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    let me = rustix::process::getuid().as_raw();
    meta.file_type().is_dir() && meta.uid() == me && (meta.permissions().mode() & 0o022) == 0
}

#[cfg(not(unix))]
fn is_safe_private_dir(dir: &Path) -> bool {
    dir.is_dir()
}

fn write_private_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let dst = dir.join(name);
    if let Err(e) = std::fs::rename(&tmp, &dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Write or refresh the `ready` heartbeat file so the hub knows Desktop's step-up bridge is live.
pub fn write_heartbeat(hub_home: &Path) -> io::Result<()> {
    let dir = hub_home.join(DIR);
    make_private_dir(&dir)?;
    let content = format!("{}\n", now_secs());
    write_private_atomic(&dir, READY_FILE, content.as_bytes())
}

pub fn remove_heartbeat(hub_home: &Path) {
    let _ = std::fs::remove_file(hub_home.join(DIR).join(READY_FILE));
}

fn valid_req_stem(stem: &str) -> bool {
    stem.len() == 32
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_nonce(n: &str) -> bool {
    (32..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Process any pending `<id>.req` files once. Returns the number of requests answered.
pub fn poll_once(hub_home: &Path, approver: &dyn Approver) -> usize {
    let dir = hub_home.join(DIR);
    if !is_safe_private_dir(&dir) {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut handled = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(req_id) = name.strip_suffix(".req") else {
            continue;
        };
        if !valid_req_stem(req_id) {
            continue;
        }
        if !is_safe_private_file(&path) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let raw = std::fs::read(&path).ok();
        // Always unlink before prompting so a single request file can never prompt twice.
        let _ = std::fs::remove_file(&path);
        let Some(raw) = raw else { continue };
        if raw.len() > 8192 {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(id_field) = v.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(nonce) = v.get("nonce").and_then(|x| x.as_str()) else {
            continue;
        };
        let reason = v
            .get("reason")
            .and_then(|x| x.as_str())
            .unwrap_or("Approve sensitive action in Playbox");
        let exp = v.get("exp").and_then(|x| x.as_u64()).unwrap_or(0);
        if id_field != req_id || !valid_nonce(nonce) || exp < now_secs() {
            continue;
        }
        let response = match approver.approve(reason) {
            Ok(true) => serde_json::json!({
                "ok": true,
                "proof": compute_proof(nonce),
            }),
            Ok(false) => serde_json::json!({
                "ok": false,
                "error": "Touch ID / owner verification was cancelled",
            }),
            Err(err) => serde_json::json!({
                "ok": false,
                "error": sanitize_reason(&err),
            }),
        };
        if let Ok(bytes) = serde_json::to_vec(&response) {
            let _ = write_private_atomic(&dir, &format!("{req_id}.res"), &bytes);
            handled += 1;
        }
    }
    handled
}

struct Signal {
    stop: AtomicBool,
    wake: (Mutex<()>, Condvar),
}

/// Background owner step-up bridge tied to the app's lifetime.
pub struct Bridge {
    hub_home: PathBuf,
    sig: Arc<Signal>,
    thread: Option<JoinHandle<()>>,
}

impl Bridge {
    pub fn start<A: Approver>(hub_home: PathBuf, approver: A) -> Self {
        if approver.available() {
            let _ = write_heartbeat(&hub_home);
        } else {
            remove_heartbeat(&hub_home);
        }
        let sig = Arc::new(Signal {
            stop: AtomicBool::new(false),
            wake: (Mutex::new(()), Condvar::new()),
        });
        let sig2 = Arc::clone(&sig);
        let home2 = hub_home.clone();
        let thread = thread::spawn(move || {
            let mut last_beat = Instant::now();
            while !sig2.stop.load(Ordering::Acquire) {
                if last_beat.elapsed() >= HEARTBEAT_EVERY {
                    if approver.available() {
                        let _ = write_heartbeat(&home2);
                    } else {
                        remove_heartbeat(&home2);
                    }
                    last_beat = Instant::now();
                }
                if approver.available() {
                    let _ = poll_once(&home2, &approver);
                }
                let (lock, cv) = &sig2.wake;
                if let Ok(g) = lock.lock() {
                    if !sig2.stop.load(Ordering::Acquire) {
                        let _ = cv.wait_timeout(g, POLL_EVERY);
                    }
                }
            }
            remove_heartbeat(&home2);
        });
        Self {
            hub_home,
            sig,
            thread: Some(thread),
        }
    }

    pub fn stop(&mut self) {
        self.sig.stop.store(true, Ordering::Release);
        self.sig.wake.1.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        remove_heartbeat(&self.hub_home);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct StubApprover {
        result: Result<bool, String>,
        calls: Arc<AtomicUsize>,
    }

    impl Approver for StubApprover {
        fn approve(&self, _reason: &str) -> Result<bool, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    fn tmp_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("pbd-stepup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        make_private_dir(&p).unwrap();
        p
    }

    #[test]
    fn heartbeat_creates_private_ready_file_and_stop_removes_it() {
        let home = tmp_home("beat");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut b = Bridge::start(
            home.clone(),
            StubApprover {
                result: Ok(true),
                calls,
            },
        );
        let ready = home.join(DIR).join(READY_FILE);
        assert!(ready.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                ready.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        b.stop();
        assert!(!ready.exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn valid_request_is_answered_with_matching_sha256_proof_once() {
        let home = tmp_home("ok");
        let dir = home.join(DIR);
        make_private_dir(&dir).unwrap();
        let req_id = "0123456789abcdef0123456789abcdef";
        let nonce = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123456789-_";
        let req = serde_json::json!({
            "id": req_id,
            "nonce": nonce,
            "reason": "Revoke member",
            "exp": now_secs() + 60,
        });
        write_private_atomic(
            &dir,
            &format!("{req_id}.req"),
            &serde_json::to_vec(&req).unwrap(),
        )
        .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let approver = StubApprover {
            result: Ok(true),
            calls: Arc::clone(&calls),
        };
        assert_eq!(poll_once(&home, &approver), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!dir.join(format!("{req_id}.req")).exists());

        let res_bytes = std::fs::read(dir.join(format!("{req_id}.res"))).unwrap();
        let res: serde_json::Value = serde_json::from_slice(&res_bytes).unwrap();
        assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            res.get("proof").and_then(|v| v.as_str()),
            Some(compute_proof(nonce).as_str())
        );

        // A second poll does nothing because `.req` was consumed.
        assert_eq!(poll_once(&home, &approver), 0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cancelled_prompt_writes_negative_response_without_proof() {
        let home = tmp_home("cancel");
        let dir = home.join(DIR);
        make_private_dir(&dir).unwrap();
        let req_id = "fedcba9876543210fedcba9876543210";
        let nonce = "nonce_0123456789_abcdefghijklmnop";
        let req = serde_json::json!({
            "id": req_id,
            "nonce": nonce,
            "reason": "Approve call",
            "exp": now_secs() + 60,
        });
        write_private_atomic(
            &dir,
            &format!("{req_id}.req"),
            &serde_json::to_vec(&req).unwrap(),
        )
        .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let approver = StubApprover {
            result: Ok(false),
            calls,
        };
        assert_eq!(poll_once(&home, &approver), 1);
        let res: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{req_id}.res"))).unwrap())
                .unwrap();
        assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert!(res.get("proof").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn expired_or_world_readable_requests_are_dropped_without_prompting() {
        let home = tmp_home("bad");
        let dir = home.join(DIR);
        make_private_dir(&dir).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let approver = StubApprover {
            result: Ok(true),
            calls: Arc::clone(&calls),
        };

        // Expired request
        let exp_id = "11111111111111111111111111111111";
        let req = serde_json::json!({
            "id": exp_id,
            "nonce": "nonce_0123456789_abcdefghijklmnop",
            "reason": "Old",
            "exp": now_secs().saturating_sub(5),
        });
        write_private_atomic(
            &dir,
            &format!("{exp_id}.req"),
            &serde_json::to_vec(&req).unwrap(),
        )
        .unwrap();
        assert_eq!(poll_once(&home, &approver), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!dir.join(format!("{exp_id}.req")).exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let open_id = "22222222222222222222222222222222";
            let ok_req = serde_json::json!({
                "id": open_id,
                "nonce": "nonce_0123456789_abcdefghijklmnop",
                "reason": "Open perms",
                "exp": now_secs() + 60,
            });
            let p = dir.join(format!("{open_id}.req"));
            std::fs::write(&p, serde_json::to_vec(&ok_req).unwrap()).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(poll_once(&home, &approver), 0);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(!p.exists());
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn sanitize_reason_strips_unicode_bidi_and_control_chars() {
        let hostile = "Revoke \u{202E}evil\u{202C}\n\tmember\u{200B}";
        let clean = sanitize_reason(hostile);
        assert_eq!(clean, "Revoke evil member");
        assert!(!clean.chars().any(is_bidi_or_format));
    }

    #[test]
    fn unavailable_approver_does_not_advertise_ready_heartbeat() {
        struct NoApprover;
        impl Approver for NoApprover {
            fn available(&self) -> bool {
                false
            }
            fn approve(&self, _reason: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let home = tmp_home("noavail");
        let _ = write_heartbeat(&home);
        let mut b = Bridge::start(home.clone(), NoApprover);
        assert!(!home.join(DIR).join(READY_FILE).exists());
        b.stop();
        let _ = std::fs::remove_dir_all(&home);
    }
}
