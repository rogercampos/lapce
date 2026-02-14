// Re-export `tracing` crate items under Lapce-specific names. `TraceLevel` avoids
// collisions with other `Level` types. `trace` (an alias for `event!`) is the primary
// logging macro used throughout the app. This module is glob-imported as `use crate::tracing::*`
// in most files for convenience.
pub use tracing::{
    self, Instrument, Level as TraceLevel, event as trace, instrument,
};
