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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_at_one() {
        let counter = Counter::new();
        assert_eq!(counter.next(), 1);
    }

    #[test]
    fn next_increments() {
        let counter = Counter::new();
        assert_eq!(counter.next(), 1);
        assert_eq!(counter.next(), 2);
        assert_eq!(counter.next(), 3);
    }

    #[test]
    fn default_starts_at_one() {
        let counter = Counter::default();
        assert_eq!(counter.next(), 1);
    }

    #[test]
    fn next_returns_value_before_increment() {
        // fetch_add returns the previous value, so the first call returns 1
        // and the internal state becomes 2
        let counter = Counter::new();
        let first = counter.next();
        let second = counter.next();
        assert_eq!(second - first, 1);
    }
}
