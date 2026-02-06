use serde_json::Value;

#[derive(Clone, Default)]
pub struct CodeLensData;

impl CodeLensData {
    pub fn new() -> Self {
        Self
    }

    pub fn run(&self, command: &str, _args: Vec<Value>) {
        tracing::debug!("todo {:}", command);
    }
}
