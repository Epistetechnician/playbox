//! Playbox Desktop core. Everything that decides how the desktop app behaves lives here, with no GUI dependency, so it can be tested without a
//! window: which directories it uses, who owns a service, how a crashing service is restarted, when a service counts as healthy, and what the
//! tray should say.
//!
//! The design rules, in order of importance:
//! 1. **A failure is information, not an emergency.** A service that dies is restarted with growing delays; one that keeps dying trips a
//!    circuit breaker and waits for a person, instead of spinning. Nothing in this crate panics on a bad environment.
//! 2. **Never fight for something already running.** If a healthy hub already answers (an older install, a LaunchAgent, a second launch),
//!    attach to it and watch it; take it over only if it stops answering.
//! 3. **Existing state is sacred.** The hub keeps its passkeys and rooms in its own home; the desktop app uses that same home and never
//!    creates a second one.
//! 4. **One interface per operating system.** Anything that differs between macOS, Linux and Windows sits behind a small function or trait
//!    here, so the rest of the app has no `cfg` in it.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod autostart;
pub mod backoff;
pub mod claim;
pub mod hub;
pub mod identity;
pub mod layout;
pub mod probe;
pub mod signin;
pub mod status;
pub mod supervisor;
pub mod update;
