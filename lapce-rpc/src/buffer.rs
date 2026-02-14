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
