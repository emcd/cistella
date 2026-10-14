//! Seat-ambient serialization: `SSH_AUTH_SOCK` is process-global
//! to the test binary, so every test that mutates it holds this
//! lock for the whole mutation window. Parallel tests otherwise
//! swap each other's agent paths mid-assertion (flake, not
//! product). The guard restores the prior value on drop while
//! still holding the lock.

use std::sync::{Mutex, MutexGuard};

static AMBIENT_LOCK: Mutex<()> = Mutex::new(());

/// Holds the ambient lock and a saved `SSH_AUTH_SOCK` value.
/// Restores on drop (before unlock — field drop order runs
/// after [`Drop::drop`]).
pub struct AmbientGuard {
    prev: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl AmbientGuard {
    /// Sets ambient to `value`, saving the prior state.
    pub fn set(value: &std::ffi::OsStr) -> Self {
        let lock = AMBIENT_LOCK.lock().expect("ambient lock");
        let prev = std::env::var("SSH_AUTH_SOCK").ok();
        unsafe { std::env::set_var("SSH_AUTH_SOCK", value) };
        Self { prev, _lock: lock }
    }

    /// Clears ambient, saving the prior state.
    pub fn unset() -> Self {
        let lock = AMBIENT_LOCK.lock().expect("ambient lock");
        let prev = std::env::var("SSH_AUTH_SOCK").ok();
        unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
        Self { prev, _lock: lock }
    }
}

impl Drop for AmbientGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var("SSH_AUTH_SOCK", value) },
            None => unsafe { std::env::remove_var("SSH_AUTH_SOCK") },
        }
    }
}
