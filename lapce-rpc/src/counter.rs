use std::sync::atomic::{self, AtomicU64};

/// Thread-safe monotonically increasing counter for generating unique IDs.
/// Starts at 1 (not 0) so that ID 0 can serve as a sentinel/uninitialized value.
/// Uses Relaxed ordering because uniqueness only requires atomicity, not
/// happens-before relationships with other memory operations.
pub struct Counter(AtomicU64);

impl Counter {
    pub const fn new() -> Counter {
        Counter(AtomicU64::new(1))
    }

    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, atomic::Ordering::Relaxed)
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}
