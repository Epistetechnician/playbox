//! Tests of the Linux confinement: the jail and the run cgroup. They fork and unshare, which needs a single-threaded process,
//! so this test binary has its own `main` (no libtest). Each case re-runs this binary as a child and reads its exit code.
//!
//! `PLAYBOX_CONFINE_ONLY=jail` runs the cases whose name contains that word; `PLAYBOX_CONFINE_REQUIRE_ALL=1` turns a skip into a
//! failure. The jail cases run rootless when started as an ordinary user and as root otherwise, and skip with a message (not a pass)
//! when the kernel refuses user namespaces. The cgroup cases need root and a writable cgroup v2 hierarchy.

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("confine: Linux only, skipped");
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use playbox_runner::{cgroup, jail::Jail};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::process::{exit, Command};

    /// A case that cannot run here. It is a pass with a loud note by default, and a failure when `PLAYBOX_CONFINE_REQUIRE_ALL` is
    /// set (CI sets it), so a skipped check can never look like a passed one where the environment is meant to support it.
    fn skip(why: &str) -> bool {
        println!("      SKIP: {why}");
        std::env::var_os("PLAYBOX_CONFINE_REQUIRE_ALL").is_none()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("playbox-confine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Run case `name` in a child of this binary; its exit code is the jailed process's.
    fn child(name: &str, env: &[(&str, String)]) -> std::process::Output {
        Command::new(std::env::current_exe().unwrap())
            .arg("--child")
            .arg(name)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .output()
            .unwrap()
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("--child") {
            exit(run_child(&args[1]));
        }
        let mut failed = 0;
        for (name, f) in [
            ("a jailed process sees only what it was given", jail_confines as fn() -> bool),
            ("a jailed process with a network keeps it but not the host's files", jail_with_network),
            ("a jail with a bad path is refused", jail_refuses_bad_paths),
            ("a run cgroup caps memory and processes and is removed", cgroup_limits),
            ("a cgroup left by a dead supervisor is found", cgroup_stale),
            ("the network filter rejects loopback from a run cgroup and only from it", nft_filter),
        ] {
            if std::env::var("PLAYBOX_CONFINE_ONLY").is_ok_and(|only| !name.contains(only.as_str())) {
                continue;
            }
            let ok = f();
            println!("{} {name}", if ok { "ok  " } else { "FAIL" });
            failed += i32::from(!ok);
        }
        exit(failed);
    }

    fn jail_confines() -> bool {
        let secret_dir = tmp("secret");
        fs::write(secret_dir.join("key"), "host secret").unwrap();
        let ro = tmp("ro");
        fs::write(ro.join("file"), "readable").unwrap();
        let rw = tmp("rw");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let out = child(
            "confined",
            &[("PB_SECRET", secret_dir.display().to_string()), ("PB_RO", ro.display().to_string()), ("PB_RW", rw.display().to_string()), ("PB_PORT", port.to_string()), ("PB_ROOT", tmp("root").display().to_string())],
        );
        report(&out)
    }

    fn jail_with_network() -> bool {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let secret_dir = tmp("secret2");
        fs::write(secret_dir.join("key"), "host secret").unwrap();
        let accept = std::thread::spawn(move || listener.accept().is_ok());
        let out = child(
            "networked",
            &[("PB_SECRET", secret_dir.display().to_string()), ("PB_PORT", port.to_string()), ("PB_ROOT", tmp("root2").display().to_string())],
        );
        report(&out) && accept.join().unwrap()
    }

    fn jail_refuses_bad_paths() -> bool {
        let out = child("badpath", &[("PB_ROOT", tmp("root3").display().to_string())]);
        report(&out)
    }

    fn report(out: &std::process::Output) -> bool {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("SKIP:") {
            return skip(stderr.trim().trim_start_matches("SKIP:").trim());
        }
        if !out.status.success() {
            println!("      exit {:?}: {}", out.status.code(), stderr.trim());
        }
        out.status.success()
    }

    fn run_child(case: &str) -> i32 {
        let get = |k: &str| PathBuf::from(std::env::var(k).unwrap_or_default());
        let jail = |network: bool, ro: Vec<PathBuf>, rw: Vec<PathBuf>| Jail { dir: get("PB_ROOT"), read_only: ro, read_write: rw, devices: vec![], network };
        let entered = |j: Jail| -> Result<(), String> { j.enter().map_err(|e| format!("{e:#}")) };
        match case {
            "confined" => {
                if let Err(e) = entered(jail(false, vec![get("PB_RO")], vec![get("PB_RW")])) {
                    if e.contains("unshare") {
                        eprintln!("SKIP: {e}");
                        return 0;
                    }
                    eprintln!("enter: {e}");
                    return 1;
                }
                let mut bad = Vec::new();
                if fs::read_to_string(get("PB_SECRET").join("key")).is_ok() {
                    bad.push("read the host secret");
                }
                if fs::read_to_string(get("PB_RO").join("file")).ok().as_deref() != Some("readable") {
                    bad.push("could not read the read-only share");
                }
                if fs::write(get("PB_RO").join("new"), "x").is_ok() {
                    bad.push("wrote to the read-only share");
                }
                if fs::write(get("PB_RW").join("new"), "x").is_err() {
                    bad.push("could not write the read-write share");
                }
                let port: u16 = std::env::var("PB_PORT").unwrap().parse().unwrap();
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    bad.push("reached the host's loopback");
                }
                if std::process::id() != 1 {
                    bad.push("is not PID 1 of its namespace");
                }
                let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
                if !status.lines().any(|l| l.starts_with("NoNewPrivs:") && l.trim_end().ends_with('1')) {
                    bad.push("can still gain privileges");
                }
                let pids = fs::read_dir("/proc").unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())).count();
                if pids > 2 {
                    bad.push("sees other processes");
                }
                if fs::read_dir("/").unwrap().filter_map(Result::ok).any(|e| matches!(e.file_name().to_str(), Some("home" | "Users" | "etc" | "usr" | "root"))) {
                    bad.push("sees the host's top-level directories");
                }
                if bad.is_empty() {
                    0
                } else {
                    eprintln!("jailed process {}", bad.join("; "));
                    1
                }
            }
            "networked" => {
                if let Err(e) = entered(jail(true, vec![], vec![])) {
                    if e.contains("unshare") {
                        eprintln!("SKIP: {e}");
                        return 0;
                    }
                    eprintln!("enter: {e}");
                    return 1;
                }
                let port: u16 = std::env::var("PB_PORT").unwrap().parse().unwrap();
                let mut s = match TcpStream::connect(("127.0.0.1", port)) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("a jail with a network could not connect: {e}");
                        return 1;
                    }
                };
                let _ = s.write_all(b"x");
                let _ = s.read(&mut [0u8; 1]);
                i32::from(fs::read_to_string(get("PB_SECRET").join("key")).is_ok())
            }
            "badpath" => {
                for p in ["relative/path", "/a/../etc", "/does/not/exist"] {
                    let j = jail(false, vec![PathBuf::from(p)], vec![]);
                    match j.enter() {
                        Err(e) if format!("{e:#}").contains("unshare") => {
                            eprintln!("SKIP: {e:#}");
                            return 0;
                        }
                        Err(_) => return 0, // refused, in the jailed child; the original process reports its code
                        Ok(()) => {
                            eprintln!("accepted the bad path {p}");
                            return 1;
                        }
                    }
                }
                1
            }
            other => {
                eprintln!("unknown case {other}");
                2
            }
        }
    }

    /// The cgroup this process runs in, as a path below the hierarchy's real root. With a private cgroup namespace (Docker's
    /// default) that is `/` and nftables, which resolves paths in the host's view, would not find what we create; run the test
    /// container with `--cgroupns=host` so the two agree.
    fn own_cgroup() -> String {
        fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string)))
            .unwrap_or_else(|| "/".into())
    }

    /// Set up once: the first call moves this process into a leaf, which changes what `own_cgroup` answers afterwards.
    fn cgroup_root() -> Result<PathBuf, String> {
        static ROOT: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
        ROOT.get_or_init(cgroup_root_once).clone()
    }

    fn cgroup_root_once() -> Result<PathBuf, String> {
        // `PLAYBOX_TEST_CGROUP_BASE=/` puts the test cgroups under the real root, which may hold processes (CI); otherwise they go
        // under this process's own cgroup, as in a container.
        let base = std::env::var("PLAYBOX_TEST_CGROUP_BASE").unwrap_or_else(|_| own_cgroup());
        let root = PathBuf::from("/sys/fs/cgroup").join(base.trim_start_matches('/'));
        if !rustix::process::getuid().is_root() {
            return Err("needs root".into());
        }
        if !root.join("cgroup.controllers").is_file() {
            return Err("no cgroup v2 hierarchy at /sys/fs/cgroup".into());
        }
        // Inside a container the cgroup we start in holds this process, and cgroup v2 forbids enabling controllers for children of
        // a cgroup that has processes: move into a leaf first, as an init system would.
        let leaf = root.join("playbox-test-leaf");
        if base == own_cgroup() && base != "/" || std::env::var_os("PLAYBOX_TEST_MOVE_ALL_PROCS").is_some() {
            let _ = fs::create_dir(&leaf);
            fs::write(leaf.join("cgroup.procs"), std::process::id().to_string()).map_err(|e| format!("moving into a leaf cgroup: {e}"))?;
        }
        // The shells that started this test are in the root cgroup too. Moving every process there is only right in a throwaway
        // container, so it needs an explicit opt-in; on a real host the test would skip instead of touching other processes.
        if leaf.is_dir() && std::env::var_os("PLAYBOX_TEST_MOVE_ALL_PROCS").is_some() {
            for pid in fs::read_to_string(root.join("cgroup.procs")).unwrap_or_default().split_whitespace() {
                let _ = fs::write(leaf.join("cgroup.procs"), pid);
            }
        }
        let r = root.join("playbox-test");
        cgroup::prepare_root(&r).map_err(|e| format!("{e:#} (controllers here: {})", fs::read_to_string(root.join("cgroup.controllers")).unwrap_or_default().trim()))?;
        Ok(r)
    }

    fn cgroup_limits() -> bool {
        let root = match cgroup_root() {
            Ok(r) => r,
            Err(e) => {
                return skip(&e);
            }
        };
        let c = match cgroup::RunCgroup::create(&root, "run-limits", cgroup::Limits { cpus: 1.5, guest_memory_mib: 64 }) {
            Ok(c) => c,
            Err(e) => {
                return skip(&format!("{e:#}"));
            }
        };
        let read = |f: &str| fs::read_to_string(c.path().join(f)).unwrap().trim().to_string();
        let ok = read("memory.max") == ((64u64 << 20) + cgroup::OVERHEAD_BYTES).to_string()
            && read("pids.max") == "512"
            && read("cpu.max") == "150000 100000"
            && read("memory.swap.max") == "0";
        // A process moved in dies with the cgroup.
        let mut sleeper = Command::new("sleep").arg("60").spawn().unwrap();
        c.add(sleeper.id()).unwrap();
        let in_cgroup = c.populated();
        let path = c.path().to_path_buf();
        drop(c);
        let reaped = sleeper.wait().map(|s| !s.success()).unwrap_or(false);
        let removed = !path.exists();
        let bad_name = cgroup::RunCgroup::create(&root, "../x", cgroup::Limits { cpus: 1.0, guest_memory_mib: 64 }).is_err();
        let _ = fs::remove_dir(&root);
        ok && in_cgroup && reaped && removed && bad_name
    }

    /// The generated rules loaded into the kernel, and a process in the run's cgroup cannot reach the host's loopback while one
    /// outside it can. Needs root, cgroup v2, `nft` and a kernel with nf_tables cgroup matching; skips with the reason otherwise.
    fn nft_filter() -> bool {
        use playbox_runner::netpolicy::{base_ruleset, run_chain, Level};
        let root = match cgroup_root() {
            Ok(r) => r,
            Err(e) => {
                return skip(&e);
            }
        };
        let nft = |input: &str| -> Result<(), String> {
            let mut c = Command::new("nft").args(["-f", "-"]).stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("nft: {e}"))?;
            c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let out = c.wait_with_output().unwrap();
            if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
        };
        let rel = root.strip_prefix("/sys/fs/cgroup").unwrap().to_str().unwrap().to_string();
        if std::env::var_os("PLAYBOX_TEST_CGROUP_BASE").is_none() && own_cgroup() == "/" {
            return skip("private cgroup namespace; run with --cgroupns=host so nftables sees the same paths");
        }
        let run = match cgroup::RunCgroup::create(&root, "deadbeef0123", cgroup::Limits { cpus: 1.0, guest_memory_mib: 64 }) {
            Ok(c) => c,
            Err(e) => {
                return skip(&format!("{e:#}"));
            }
        };
        let rules = format!("{}{}", base_ruleset(&rel).unwrap(), "");
        if let Err(e) = nft(&rules) {
            return skip(&format!("loading the base table: {e}"));
        }
        let chain = run_chain(&rel, "deadbeef0123", Level::Internet, &[]).unwrap();
        let loaded = nft(&chain);
        let result = (|| -> Result<bool, String> {
            loaded?;
            let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || while listener.accept().is_ok() {});
            let probe = |in_cgroup: bool| -> bool {
                let mut c = Command::new("sh");
                let script = if in_cgroup { format!("echo $$ > {}/cgroup.procs; exec timeout 3 bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'", run.path().display()) } else { format!("exec timeout 3 bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'") };
                c.args(["-c", &script]).status().map(|s| s.success()).unwrap_or(false)
            };
            let outside = probe(false);
            let inside = probe(true);
            if !outside {
                return Err("a process outside the run cgroup could not reach loopback, so the test proves nothing".into());
            }
            Ok(!inside)
        })();
        let _ = Command::new("nft").args(["delete", "table", "inet", "playbox_runner"]).status();
        drop(run);
        let _ = fs::remove_dir(&root);
        match result {
            Ok(blocked) => blocked,
            Err(e) => skip(&e),
        }
    }

    fn cgroup_stale() -> bool {
        let root = match cgroup_root() {
            Ok(r) => r,
            Err(e) => {
                return skip(&e);
            }
        };
        let live = match cgroup::RunCgroup::create(&root, "run-live", cgroup::Limits { cpus: 1.0, guest_memory_mib: 64 }) {
            Ok(c) => c,
            Err(e) => {
                return skip(&format!("{e:#}"));
            }
        };
        // A directory nobody holds a lock on stands in for one a dead supervisor left.
        fs::create_dir(root.join("run-dead")).unwrap();
        let found = cgroup::stale(&root).unwrap();
        let ok = found == vec![root.join("run-dead")];
        cgroup::remove(&root.join("run-dead")).unwrap();
        drop(live);
        let _ = fs::remove_dir(&root);
        ok
    }
}
