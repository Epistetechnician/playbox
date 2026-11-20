//! Keeps one child service running.
//!
//! ```text
//!   Starting ──healthy──▶ Healthy ──exit / hung──▶ Backoff ──delay──▶ Starting …
//!      │                                              │
//!      └── too many quick failures in a window ──▶ Failed (circuit open) ──reset()──▶ Starting
//!   Attached: a healthy service we did not start; watched, never killed, taken over only if it stops answering.
//! ```
//!
//! * A child that exits or stops answering its probe is restarted after a growing delay (`Backoff`).
//! * A run that lasted at least `stable_after` forgets earlier failures; a service that fails more than `max_failures` times inside
//!   `window` opens the circuit and stays down until `reset()`, so a broken install never becomes a busy loop.
//! * Stopping sends a polite terminate first, then kills the whole process group after `grace`, so a hub does not leave its tunnel behind.
//! * All waiting is interruptible: `stop()` returns within about a poll interval, not after the current delay.

use crate::backoff::Backoff;
use crate::probe::Probe;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Healthy,
    /// A healthy service that was already running when we looked: watched, not owned.
    Attached,
    Backoff {
        attempt: u32,
        retry_in: Duration,
    },
    /// The circuit is open: too many failures too quickly. Needs `reset()`.
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct Event {
    pub name: String,
    pub state: State,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub backoff: Backoff,
    /// A run at least this long counts as stable: earlier failures are forgotten.
    pub stable_after: Duration,
    pub max_failures: u32,
    pub window: Duration,
    /// How long a new child has to become healthy.
    pub start_timeout: Duration,
    pub health_interval: Duration,
    pub probe_timeout: Duration,
    /// Consecutive failed probes of a once-healthy child before it is restarted.
    pub unhealthy_after: u32,
    /// Time between the polite terminate and the kill.
    pub grace: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            backoff: Backoff::default(),
            stable_after: Duration::from_secs(60),
            max_failures: 5,
            window: Duration::from_secs(120),
            start_timeout: Duration::from_secs(30),
            health_interval: Duration::from_secs(5),
            probe_timeout: Duration::from_secs(2),
            unhealthy_after: 3,
            grace: Duration::from_secs(8),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Where the child's output goes (appended); `None` discards it.
    pub log: Option<PathBuf>,
    pub health: Option<Probe>,
    /// If the probe already passes when we start, watch that service instead of starting another.
    pub attach: bool,
    pub policy: Policy,
}

impl Spec {
    pub fn new(name: &str, program: impl Into<PathBuf>) -> Self {
        Self {
            name: name.to_string(),
            program: program.into(),
            args: vec![],
            env: vec![],
            env_remove: vec![],
            cwd: None,
            log: None,
            health: None,
            attach: false,
            policy: Policy::default(),
        }
    }
}

#[derive(Default)]
struct Inner {
    state: Option<State>,
    stop: bool,
    reset: bool,
    restart: bool,
}

struct Shared {
    m: Mutex<Inner>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.m.lock().unwrap_or_else(|p| p.into_inner()) // a panic elsewhere must not take supervision down with it
    }
    fn stopping(&self) -> bool {
        self.lock().stop
    }
    /// Sleep up to `d`, waking early on stop, restart or reset. Returns true if woken by one of them.
    fn nap(&self, d: Duration) -> bool {
        let g = self.lock();
        if g.stop || g.restart || g.reset {
            return true;
        }
        let (g, _) = self
            .cv
            .wait_timeout(g, d)
            .unwrap_or_else(|p| p.into_inner());
        g.stop || g.restart || g.reset
    }
}

pub struct Supervisor {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// Start supervising. `on_event` is called from the supervisor's thread on every state change.
    pub fn start(spec: Spec, on_event: impl Fn(Event) + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            m: Mutex::new(Inner::default()),
            cv: Condvar::new(),
        });
        let sh = shared.clone();
        let join = thread::Builder::new()
            .name(format!("supervise-{}", spec.name))
            .spawn(move || {
                Runner {
                    spec,
                    sh,
                    on_event: Box::new(on_event),
                    last: None,
                }
                .run()
            })
            .ok();
        Self { shared, join }
    }

    pub fn state(&self) -> State {
        self.shared.lock().state.clone().unwrap_or(State::Starting)
    }

    /// Close an open circuit and try again now.
    pub fn reset(&self) {
        self.shared.lock().reset = true;
        self.shared.cv.notify_all();
    }

    /// Restart the child now (it is stopped politely, then started again).
    pub fn restart(&self) {
        self.shared.lock().restart = true;
        self.shared.cv.notify_all();
    }

    /// Stop supervising and stop the child. Blocks until it has been stopped.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.lock().stop = true;
        self.shared.cv.notify_all();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum Outcome {
    Stop,
    Restart,
    Died(String),
    Unhealthy,
    NeverHealthy,
}

struct Runner {
    spec: Spec,
    sh: Arc<Shared>,
    on_event: Box<dyn Fn(Event) + Send>,
    last: Option<State>,
}

impl Runner {
    fn set(&mut self, state: State, detail: impl Into<String>) {
        self.sh.lock().state = Some(state.clone());
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state.clone());
            (self.on_event)(Event {
                name: self.spec.name.clone(),
                state,
                detail: detail.into(),
            });
        }
    }

    fn run(mut self) {
        let p = self.spec.policy.clone();
        let mut backoff = p.backoff.clone();
        let mut failures: VecDeque<Instant> = VecDeque::new();
        self.set(State::Starting, "starting");

        if self.spec.attach && self.attach_and_watch() {
            self.set(State::Stopped, "stopped");
            return;
        }

        while !self.sh.stopping() {
            self.set(State::Starting, "starting");
            let started = Instant::now();
            let (outcome, detail) = match self.spawn() {
                Err(e) => (Outcome::Died(e.clone()), e),
                Ok(mut child) => {
                    let out = self.watch(&mut child);
                    terminate(&mut child, p.grace);
                    let d = match &out {
                        Outcome::Stop => "stopped".to_string(),
                        Outcome::Restart => "restart requested".to_string(),
                        Outcome::Died(s) => s.clone(),
                        Outcome::Unhealthy => "stopped answering its health check".to_string(),
                        Outcome::NeverHealthy => {
                            format!("not healthy after {}s", p.start_timeout.as_secs())
                        }
                    };
                    (out, d)
                }
            };
            match outcome {
                Outcome::Stop => break,
                Outcome::Restart => {
                    self.sh.lock().restart = false;
                    backoff.reset();
                    continue;
                }
                _ => {}
            }
            if started.elapsed() >= p.stable_after {
                backoff.reset();
                failures.clear();
            }
            let now = Instant::now();
            failures.push_back(now);
            while failures
                .front()
                .is_some_and(|t| now.duration_since(*t) > p.window)
            {
                failures.pop_front();
            }
            if failures.len() as u32 > p.max_failures {
                let reason = format!(
                    "{} failures in {}s; last: {detail}",
                    failures.len(),
                    p.window.as_secs()
                );
                self.set(
                    State::Failed {
                        reason: reason.clone(),
                    },
                    reason,
                );
                while !self.sh.stopping() {
                    if self.sh.lock().reset {
                        break;
                    }
                    self.sh.nap(Duration::from_millis(200));
                }
                let mut g = self.sh.lock();
                g.reset = false;
                drop(g);
                failures.clear();
                backoff.reset();
                continue;
            }
            let delay = backoff.next_delay();
            self.set(
                State::Backoff {
                    attempt: backoff.attempt(),
                    retry_in: delay,
                },
                format!("{detail}; retrying in {}s", delay.as_secs_f32().ceil()),
            );
            let deadline = Instant::now() + delay;
            while !self.sh.stopping() && Instant::now() < deadline {
                if self.sh.nap(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(250)),
                ) {
                    let mut g = self.sh.lock();
                    if g.reset || g.restart {
                        g.reset = false;
                        g.restart = false;
                        drop(g);
                        backoff.reset();
                        break;
                    }
                }
            }
        }
        self.set(State::Stopped, "stopped");
    }

    /// Watch a service we did not start. Returns true if we should stop; false if it stopped answering and we should take over.
    fn attach_and_watch(&mut self) -> bool {
        let Some(probe) = self.spec.health.clone() else {
            return false;
        };
        let p = self.spec.policy.clone();
        if !probe.check(p.probe_timeout) {
            return false;
        }
        self.set(State::Attached, "already running; attached");
        let mut bad = 0;
        loop {
            if self.sh.nap(p.health_interval) && self.sh.stopping() {
                return true;
            }
            if probe.check(p.probe_timeout) {
                bad = 0;
            } else {
                bad += 1;
                if bad >= p.unhealthy_after {
                    return false;
                }
            }
        }
    }

    fn spawn(&self) -> Result<Child, String> {
        let mut c = Command::new(&self.spec.program);
        c.args(&self.spec.args);
        for k in &self.spec.env_remove {
            c.env_remove(k);
        }
        c.envs(self.spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        if let Some(d) = &self.spec.cwd {
            c.current_dir(d);
        }
        match self.spec.log.as_ref().and_then(|p| {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            OpenOptions::new().create(true).append(true).open(p).ok()
        }) {
            Some(f) => {
                if let Ok(g) = f.try_clone() {
                    c.stdout(Stdio::from(g));
                } else {
                    c.stdout(Stdio::null());
                }
                c.stderr(Stdio::from(f));
            }
            None => {
                c.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0); // its own group: a terminal's Ctrl-C does not reach it, and the whole tree can be stopped together
        }
        c.spawn()
            .map_err(|e| format!("could not start {}: {e}", self.spec.program.display()))
    }

    fn watch(&mut self, child: &mut Child) -> Outcome {
        let p = self.spec.policy.clone();
        let started = Instant::now();
        let (mut healthy, mut bad) = (false, 0u32);
        let mut last_probe: Option<Instant> = None;
        loop {
            match child.try_wait() {
                Ok(Some(st)) => return Outcome::Died(format!("exited with {st}")),
                Ok(None) => {}
                Err(e) => return Outcome::Died(format!("could not be watched: {e}")),
            }
            {
                let mut g = self.sh.lock();
                if g.stop {
                    return Outcome::Stop;
                }
                if g.restart {
                    g.restart = false;
                    return Outcome::Restart;
                }
            }
            match self.spec.health.clone() {
                None => {
                    if !healthy && started.elapsed() >= Duration::from_secs(1) {
                        healthy = true;
                        self.set(State::Healthy, "running");
                    }
                }
                Some(probe) => {
                    let due = last_probe.is_none_or(|t| {
                        t.elapsed()
                            >= if healthy {
                                p.health_interval
                            } else {
                                Duration::from_millis(250)
                            }
                    });
                    if due {
                        last_probe = Some(Instant::now());
                        let ok = probe.check(p.probe_timeout);
                        if !healthy {
                            if ok {
                                healthy = true;
                                self.set(State::Healthy, "healthy");
                            } else if started.elapsed() > p.start_timeout {
                                return Outcome::NeverHealthy;
                            }
                        } else if ok {
                            bad = 0;
                        } else {
                            bad += 1;
                            if bad >= p.unhealthy_after {
                                return Outcome::Unhealthy;
                            }
                        }
                    }
                }
            }
            self.sh.nap(Duration::from_millis(100));
        }
    }
}

/// Stop a child: terminate politely, wait `grace`, then kill its whole process group. Always reaps it.
fn terminate(child: &mut Child, grace: Duration) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    signal(child, false);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            kill_group(child);
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    kill_group(child);
    signal(child, true);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal(child: &Child, kill: bool) {
    use rustix::process::{kill_process, Pid, Signal};
    if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        let _ = kill_process(pid, if kill { Signal::KILL } else { Signal::TERM });
    }
}

#[cfg(not(unix))]
fn signal(child: &mut Child, kill: bool) {
    if kill {
        let _ = child.kill();
    } // Windows has no polite terminate for a console child; the kill is the stop
}

/// After the main process is gone, sweep anything it left in its group (a hub's tunnel).
#[cfg(unix)]
fn kill_group(child: &Child) {
    use rustix::process::{kill_process_group, Pid, Signal};
    if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_child: &Child) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::probe::testserver::serve;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    fn fast() -> Policy {
        Policy {
            backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(80)),
            stable_after: Duration::from_secs(30),
            max_failures: 3,
            window: Duration::from_secs(30),
            start_timeout: Duration::from_millis(700),
            health_interval: Duration::from_millis(120),
            probe_timeout: Duration::from_millis(200),
            unhealthy_after: 2,
            grace: Duration::from_millis(500),
        }
    }

    fn sh(name: &str, script: &str, p: Policy) -> Spec {
        let mut s = Spec::new(name, "/bin/sh");
        s.args = vec!["-c".into(), script.into()];
        s.policy = p;
        s
    }

    fn wait_for(
        rx: &mpsc::Receiver<Event>,
        secs: u64,
        f: impl Fn(&State) -> bool,
    ) -> Option<Event> {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if let Ok(e) = rx.recv_timeout(Duration::from_millis(100)) {
                if f(&e.state) {
                    return Some(e);
                }
            }
        }
        None
    }

    #[test]
    fn a_crashing_child_trips_the_circuit_and_reset_closes_it() {
        let (tx, rx) = mpsc::channel();
        let s = Supervisor::start(sh("crash", "exit 3", fast()), move |e| {
            let _ = tx.send(e);
        });
        let failed = wait_for(&rx, 10, |st| matches!(st, State::Failed { .. }))
            .expect("circuit should open");
        assert!(failed.detail.contains("failures"), "{}", failed.detail);
        // while open it does not spin: no new Starting events appear
        assert!(wait_for(&rx, 1, |st| *st == State::Starting).is_none());
        s.reset();
        assert!(
            wait_for(&rx, 5, |st| *st == State::Starting).is_some(),
            "reset should try again"
        );
        s.stop();
    }

    #[test]
    fn a_child_that_cannot_start_is_a_failure_not_a_panic() {
        let (tx, rx) = mpsc::channel();
        let mut spec = Spec::new("missing", "/definitely/not/a/program");
        spec.policy = fast();
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        let e = wait_for(&rx, 10, |st| matches!(st, State::Failed { .. })).expect("should fail");
        assert!(e.detail.contains("could not start"), "{}", e.detail);
        s.stop();
    }

    #[test]
    fn healthy_when_the_probe_passes_and_stop_ends_the_child() {
        let srv = serve(200);
        let (tx, rx) = mpsc::channel();
        let mut spec = sh("svc", "sleep 60", fast());
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| *st == State::Healthy).is_some());
        let t0 = Instant::now();
        s.stop();
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "stop must not wait out the sleep"
        );
    }

    #[test]
    fn a_hung_service_is_restarted() {
        let srv = serve(200);
        let (tx, rx) = mpsc::channel();
        let mut spec = sh("hang", "sleep 60", fast());
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| *st == State::Healthy).is_some());
        srv.status.store(0, Ordering::SeqCst); // the service stops answering while its process stays alive
        let e = wait_for(&rx, 8, |st| matches!(st, State::Backoff { .. }))
            .expect("should be restarted");
        assert!(e.detail.contains("health check"), "{}", e.detail);
        srv.status.store(200, Ordering::SeqCst);
        assert!(
            wait_for(&rx, 8, |st| *st == State::Healthy).is_some(),
            "and become healthy again"
        );
        s.stop();
    }

    #[test]
    fn a_child_that_never_becomes_healthy_is_restarted() {
        let srv = serve(503);
        let (tx, rx) = mpsc::channel();
        let mut spec = sh("slow", "sleep 60", fast());
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        let e = wait_for(&rx, 8, |st| matches!(st, State::Backoff { .. }))
            .expect("should give up on this start");
        assert!(e.detail.contains("not healthy after"), "{}", e.detail);
        s.stop();
    }

    #[test]
    fn a_service_already_running_is_attached_not_duplicated() {
        let srv = serve(200);
        let (tx, rx) = mpsc::channel();
        let mut spec = Spec::new("existing", "/definitely/not/a/program"); // if it tried to start one, this would fail
        spec.policy = fast();
        spec.attach = true;
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| *st == State::Attached).is_some());
        assert!(
            wait_for(&rx, 1, |st| matches!(
                st,
                State::Failed { .. } | State::Backoff { .. }
            ))
            .is_none(),
            "attached must stay attached"
        );
        s.stop();
    }

    #[test]
    fn an_attached_service_that_dies_is_taken_over() {
        let srv = serve(200);
        let (tx, rx) = mpsc::channel();
        let mut spec = sh("takeover", "sleep 60", fast());
        spec.attach = true;
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| *st == State::Attached).is_some());
        srv.status.store(0, Ordering::SeqCst);
        assert!(
            wait_for(&rx, 8, |st| *st == State::Starting).is_some(),
            "it should start its own once the other stops answering"
        );
        s.stop();
    }

    #[test]
    fn restart_now_replaces_the_child() {
        let srv = serve(200);
        let (tx, rx) = mpsc::channel();
        let mut spec = sh("again", "sleep 60", fast());
        spec.health = Some(Probe::http(srv.addr, "/", &[200]));
        let s = Supervisor::start(spec, move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| *st == State::Healthy).is_some());
        s.restart();
        assert!(wait_for(&rx, 5, |st| *st == State::Starting).is_some());
        assert!(wait_for(&rx, 5, |st| *st == State::Healthy).is_some());
        s.stop();
    }

    #[test]
    fn stopping_during_a_long_backoff_is_immediate() {
        let (tx, rx) = mpsc::channel();
        let mut p = fast();
        p.backoff = Backoff::new(Duration::from_secs(60), Duration::from_secs(60));
        p.max_failures = 100;
        let s = Supervisor::start(sh("longwait", "exit 1", p), move |e| {
            let _ = tx.send(e);
        });
        assert!(wait_for(&rx, 5, |st| matches!(st, State::Backoff { .. })).is_some());
        let t0 = Instant::now();
        s.stop();
        assert!(t0.elapsed() < Duration::from_secs(2));
    }
}
