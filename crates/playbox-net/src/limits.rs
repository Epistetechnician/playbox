//! Per-member abuse limits: how many tasks one member may have running, how fast they may start
//! them, how many members the room holds, and a total budget of tasks for the host's lifetime.
//!
//! Counted on attempt, so a flood of failing or refused tasks cannot be repeated for free.

use iroh::EndpointId;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct AbuseLimits {
    /// Most members the room admits (invites stop working at this count). Default 64.
    pub max_members: usize,
    /// Tasks one member may have running at once. Default 2.
    pub max_concurrent_per_member: usize,
    /// Tasks one member may start in `rate_window`. Default 30 per minute.
    pub max_tasks_per_window: usize,
    pub rate_window: Duration,
    /// Tasks the whole room may start since the host started (not saved across restarts). Default 100 000.
    pub task_budget: u64,
}

impl Default for AbuseLimits {
    fn default() -> Self {
        Self {
            max_members: 64,
            max_concurrent_per_member: 2,
            max_tasks_per_window: 30,
            rate_window: Duration::from_secs(60),
            task_budget: 100_000,
        }
    }
}

#[derive(Default, Debug)]
struct Inner {
    inflight: HashMap<EndpointId, usize>,
    recent: HashMap<EndpointId, VecDeque<Instant>>,
    started: u64,
}

#[derive(Clone)]
pub struct Limiter {
    cfg: AbuseLimits,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
/// Held while a task runs; frees the member's concurrency slot on drop.
pub struct Admission {
    who: EndpointId,
    inner: Arc<Mutex<Inner>>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut g = self.inner.lock().expect("limiter lock");
        if let Some(n) = g.inflight.get_mut(&self.who) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.inflight.remove(&self.who);
            }
        }
    }
}

/// The limit that stopped a task from being admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitError {
    /// The room has started as many tasks as its budget allows.
    Budget(u64),
    /// The member already has this many tasks running.
    Concurrency(usize),
    /// The member started too many tasks within the rate window.
    Rate { max: usize, window_secs: u64 },
}

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(n) => write!(f, "this room has used its budget of {n} tasks"),
            Self::Concurrency(n) => {
                write!(
                    f,
                    "you already have {n} tasks running; wait for one to finish"
                )
            }
            Self::Rate { max, window_secs } => {
                write!(f, "rate limit: at most {max} tasks per {window_secs}s")
            }
        }
    }
}

impl std::error::Error for LimitError {}

impl Limiter {
    #[must_use]
    pub fn new(cfg: AbuseLimits) -> Self {
        Self {
            cfg,
            inner: Arc::default(),
        }
    }

    #[must_use]
    pub fn max_members(&self) -> usize {
        self.cfg.max_members
    }

    /// Admit one task from `who`, or say which limit stopped it.
    ///
    /// # Errors
    ///
    /// Returns a [`LimitError`] naming the limit that stopped the request.
    pub fn admit(&self, who: EndpointId) -> Result<Admission, LimitError> {
        self.admit_at(who, Instant::now())
    }

    fn admit_at(&self, who: EndpointId, now: Instant) -> Result<Admission, LimitError> {
        let c = &self.cfg;
        let mut g = self.inner.lock().expect("limiter lock");
        if g.started >= c.task_budget {
            return Err(LimitError::Budget(c.task_budget));
        }
        if g.inflight.get(&who).copied().unwrap_or(0) >= c.max_concurrent_per_member {
            return Err(LimitError::Concurrency(c.max_concurrent_per_member));
        }
        let q = g.recent.entry(who).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= c.rate_window)
        {
            q.pop_front();
        }
        if q.len() >= c.max_tasks_per_window {
            return Err(LimitError::Rate {
                max: c.max_tasks_per_window,
                window_secs: c.rate_window.as_secs(),
            });
        }
        q.push_back(now);
        g.started += 1;
        *g.inflight.entry(who).or_default() += 1;
        Ok(Admission {
            who,
            inner: self.inner.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[n; 32]).public()
    }

    #[test]
    fn concurrency_is_per_member_and_released_on_drop() {
        let l = Limiter::new(AbuseLimits {
            max_concurrent_per_member: 2,
            ..Default::default()
        });
        let a1 = l.admit(id(1)).unwrap();
        let _a2 = l.admit(id(1)).unwrap();
        assert!(l
            .admit(id(1))
            .unwrap_err()
            .to_string()
            .contains("already have 2"));
        assert!(l.admit(id(2)).is_ok(), "another member is unaffected");
        drop(a1);
        assert!(l.admit(id(1)).is_ok());
    }

    #[test]
    fn rate_window_slides() {
        let l = Limiter::new(AbuseLimits {
            max_tasks_per_window: 2,
            max_concurrent_per_member: 10,
            rate_window: Duration::from_secs(60),
            ..Default::default()
        });
        let t0 = Instant::now();
        drop(l.admit_at(id(1), t0).unwrap());
        drop(l.admit_at(id(1), t0).unwrap());
        assert!(l
            .admit_at(id(1), t0 + Duration::from_secs(30))
            .unwrap_err()
            .to_string()
            .contains("rate limit"));
        assert!(l.admit_at(id(1), t0 + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn total_budget_is_shared() {
        let l = Limiter::new(AbuseLimits {
            task_budget: 2,
            max_concurrent_per_member: 10,
            ..Default::default()
        });
        drop(l.admit(id(1)).unwrap());
        drop(l.admit(id(2)).unwrap());
        assert!(l.admit(id(3)).unwrap_err().to_string().contains("budget"));
    }
}
