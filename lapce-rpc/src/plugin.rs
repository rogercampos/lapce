use serde::{Deserialize, Serialize};

use crate::counter::Counter;

/// Runtime identifier for a plugin instance. Different from VoltID (which
/// identifies a plugin by author/name). A single Volt can be reinstalled
/// or reloaded, getting a new PluginId each time. Used to route LSP responses
/// back to the correct language server instance.
#[derive(Eq, PartialEq, Hash, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PluginId(pub u64);

impl PluginId {
    pub fn next() -> Self {
        static PLUGIN_ID_COUNTER: Counter = Counter::new();
        Self(PLUGIN_ID_COUNTER.next())
    }
}
