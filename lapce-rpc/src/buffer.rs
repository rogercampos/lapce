use serde::{Deserialize, Serialize};

use crate::counter::Counter;

/// Unique identifier for a text buffer (open document). Generated via a
/// global atomic counter so each buffer gets a distinct ID across the
/// lifetime of the process, even if the same file is closed and reopened.
#[derive(Eq, PartialEq, Hash, Copy, Clone, Debug, Serialize, Deserialize)]
pub struct BufferId(pub u64);

impl BufferId {
    pub fn next() -> Self {
        static BUFFER_ID_COUNTER: Counter = Counter::new();
        Self(BUFFER_ID_COUNTER.next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_returns_unique_ids() {
        let a = BufferId::next();
        let b = BufferId::next();
        let c = BufferId::next();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn next_ids_are_sequential() {
        let a = BufferId::next();
        let b = BufferId::next();
        assert_eq!(b.0 - a.0, 1);
    }
}
