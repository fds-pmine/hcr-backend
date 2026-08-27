//! Time, injected.
//!
//! Round deadlines are decided by the **server's** clock, so that clock has to be
//! substitutable — otherwise testing a five-minute round means waiting five
//! minutes, and nobody writes that test twice.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of wall-clock time in epoch milliseconds.
pub trait Clock: Debug + Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    /// Start at `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(now_ms)))
    }

    /// Jump forward.
    pub fn advance(&self, millis: u64) {
        self.0.fetch_add(millis, Ordering::SeqCst);
    }

    /// Jump to an absolute time.
    pub fn set(&self, now_ms: u64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A shared clock handle.
pub type SharedClock = Arc<dyn Clock>;

/// The real clock, shared.
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}
