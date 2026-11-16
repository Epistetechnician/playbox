//! Lock helpers that recover a poisoned lock instead of propagating the panic.
//!
//! A panic while a lock is held poisons it. The room's state stays structurally valid (each critical
//! section is a few field updates), and one failed connection must not take the whole host down, so
//! these return the guard regardless.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub(crate) trait LockExt<T> {
    /// Locks, recovering the guard if a previous holder panicked.
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
