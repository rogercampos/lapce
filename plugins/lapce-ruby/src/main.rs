use serde_json::{json, Value};
use lapce_plugin::psp_types::lsp_types::{
    request::Initialize, DocumentFilter, DocumentSelector, InitializeParams, Url,
};
use lapce_plugin::psp_types::Request;
use lapce_plugin::{register_plugin, LapcePlugin, PLUGIN_RPC};

#[derive(Default)]
struct State;

register_plugin!(State);

impl LapcePlugin for State {
    fn handle_request(&mut self, _id: u64, method: String, params: Value) {
        if method == Initialize::METHOD {
            let _params: InitializeParams = serde_json::from_value(params).unwrap();

            let document_selector: DocumentSelector = vec![DocumentFilter {
                language: Some(String::from("ruby")),
                pattern: Some(String::from("**/*.rb")),
                scheme: None,
            }];

            let server_uri = Url::parse("urn:ruby-lsp").unwrap();

            // Disable semantic highlighting from ruby-lsp so that Lapce's
            // TreeSitter grammar highlighting is used instead.
            let options = Some(json!({
                "enabledFeatures": {
                    "semanticHighlighting": false
                }
            }));

            PLUGIN_RPC.start_lsp(
                server_uri,
                Vec::new(),
                document_selector,
                options,
            );
        }
    }
}
