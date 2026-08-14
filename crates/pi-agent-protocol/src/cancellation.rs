//! Structured, runtime-independent cancellation.
//!
//! [`CancellationSource`] owns cancellation and [`CancellationToken`] is the
//! read-only handle passed to work.  The implementation is synchronous and
//! uses only the standard library so protocol consumers can adapt it to any
//! executor.  It deliberately does not spawn a thread or prescribe how an
//! async task is woken; an adapter should call [`CancellationToken::check`]
//! at its await/yield boundaries and bridge its runtime's notification API.

use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug)]
struct CancellationInner {
    cancelled: AtomicBool,
    reason: Mutex<Option<String>>,
    wake: Condvar,
}

/// The owner side of a cancellation scope.
#[derive(Clone, Debug)]
pub struct CancellationSource {
    inner: Arc<CancellationInner>,
}

/// A cheaply clonable, read-only cancellation handle.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

/// Error returned when work observes cancellation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cancelled {
    reason: Option<String>,
}

impl CancellationSource {
    /// Create a new uncancelled scope.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                cancelled: AtomicBool::new(false),
                reason: Mutex::new(None),
                wake: Condvar::new(),
            }),
        }
    }

    /// Return a token that observes this source.
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Cancel the scope without a reason.
    ///
    /// Returns `true` only for the call that transitions the scope from live
    /// to cancelled.  Later calls are harmless and return `false`.
    pub fn cancel(&self) -> bool {
        self.cancel_with_reason("")
    }

    /// Cancel the scope and preserve the first supplied reason.
    pub fn cancel_with_reason(&self, reason: impl Into<String>) -> bool {
        if self
            .inner
            .cancelled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }

        let reason = reason.into();
        if !reason.is_empty() {
            let mut stored = lock_unpoisoned(&self.inner.reason);
            *stored = Some(reason);
        }
        self.inner.wake.notify_all();
        true
    }

    /// Whether this source has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Whether the cancellation scope has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Return an owned cancellation reason, if one was supplied.
    pub fn reason_owned(&self) -> Option<String> {
        lock_unpoisoned(&self.inner.reason).clone()
    }

    /// Return `Err` when cancelled, preserving the current reason.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled {
                reason: self.reason_owned(),
            })
        } else {
            Ok(())
        }
    }

    /// Block the current thread until cancellation is observed.
    ///
    /// This is an adapter utility for synchronous integrations, not an event
    /// loop.  Async integrations should bridge the same state to their own
    /// notification primitive instead of blocking an executor worker.
    pub fn wait_blocking(&self) -> Result<(), Cancelled> {
        let mut reason = lock_unpoisoned(&self.inner.reason);
        while !self.inner.cancelled.load(Ordering::SeqCst) {
            reason = wait_unpoisoned(&self.inner.wake, reason);
        }
        Err(Cancelled {
            reason: reason.clone(),
        })
    }
}

impl Cancelled {
    /// Return the cancellation reason, if supplied.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

impl fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.reason {
            Some(reason) => write!(formatter, "operation cancelled: {reason}"),
            None => formatter.write_str("operation cancelled"),
        }
    }
}

impl Error for Cancelled {}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_unpoisoned<'a, T>(
    wake: &Condvar,
    guard: std::sync::MutexGuard<'a, T>,
) -> std::sync::MutexGuard<'a, T> {
    wake.wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::CancellationSource;

    #[test]
    fn cancellation_is_shared_and_first_reason_wins() {
        let source = CancellationSource::new();
        let token = source.token();

        assert!(!token.is_cancelled());
        assert!(source.cancel_with_reason("shutdown"));
        assert!(!source.cancel_with_reason("late reason"));
        assert_eq!(token.reason_owned().as_deref(), Some("shutdown"));
        assert_eq!(token.check().unwrap_err().reason(), Some("shutdown"));
    }

    #[test]
    fn blocking_wait_observes_cancellation() {
        let source = CancellationSource::new();
        let token = source.token();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            source.cancel();
        });

        assert!(token.wait_blocking().is_err());
        canceller.join().expect("canceller should finish");
    }
}
