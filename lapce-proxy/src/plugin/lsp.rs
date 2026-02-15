#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{self, Child, Command, Stdio},
    sync::Arc,
    thread,
};

use anyhow::{Result, anyhow};
use jsonrpc_lite::{Id, Params};
use lapce_core::meta;
use lapce_rpc::{
    RpcError,
    plugin::{PluginId, VoltID},
    style::LineStyle,
};
use lapce_xi_rope::Rope;
use lsp_types::{
    notification::{Initialized, Notification},
    request::{Initialize, Request},
    *,
};
use parking_lot::Mutex;
use serde_json::Value;

use super::{
    client_capabilities,
    psp::{
        PluginHandlerNotification, PluginHostHandler, PluginServerHandler,
        PluginServerRpcHandler, ResponseSender, RpcCallback,
        handle_plugin_server_message,
    },
};
use crate::plugin::PluginCatalogRpcHandler;

const HEADER_CONTENT_LENGTH: &str = "content-length";
const HEADER_CONTENT_TYPE: &str = "content-type";

/// Manages a single language server process. Owns the child process handle and
/// coordinates three background threads:
/// - Writer thread: serializes JSON-RPC messages to the LSP's stdin
/// - Reader thread: reads JSON-RPC messages from the LSP's stdout
/// - Stderr thread: captures LSP stderr output and forwards to the log panel
///
/// The LSP protocol uses Content-Length framed messages over stdio, which is
/// handled by `read_message` and the writer thread's formatting logic.
pub struct LspClient {
    plugin_rpc: PluginCatalogRpcHandler,
    server_rpc: PluginServerRpcHandler,
    process: Child,
    workspace: Option<PathBuf>,
    host: PluginHostHandler,
    options: Option<Value>,
}

impl PluginServerHandler for LspClient {
    fn method_registered(&mut self, method: &str) -> bool {
        self.host.method_registered(method)
    }

    fn document_supported(
        &mut self,
        language_id: Option<&str>,
        path: Option<&Path>,
    ) -> bool {
        self.host.document_supported(language_id, path)
    }

    fn handle_handler_notification(
        &mut self,
        notification: PluginHandlerNotification,
    ) {
        use PluginHandlerNotification::*;
        match notification {
            Initialize => {
                self.initialize();
            }
            InitializeResult(result) => {
                self.host.server_capabilities = result.capabilities;
            }
            Shutdown => {
                self.shutdown();
            }
            SpawnedPluginLoaded { .. } => {}
        }
    }

    fn handle_host_request(
        &mut self,
        id: Id,
        method: String,
        params: Params,
        resp: ResponseSender,
    ) {
        self.host.handle_request(id, method, params, resp);
    }

    fn handle_host_notification(
        &mut self,
        method: String,
        params: Params,
        from: String,
    ) {
        if let Err(err) = self.host.handle_notification(method, params, from) {
            tracing::error!("{:?}", err);
        }
    }

    fn handle_did_save_text_document(
        &self,
        language_id: String,
        path: PathBuf,
        text_document: TextDocumentIdentifier,
        text: lapce_xi_rope::Rope,
    ) {
        self.host.handle_did_save_text_document(
            language_id,
            path,
            text_document,
            text,
        );
    }

    fn handle_did_change_text_document(
        &mut self,
        language_id: String,
        document: lsp_types::VersionedTextDocumentIdentifier,
        delta: lapce_xi_rope::RopeDelta,
        text: lapce_xi_rope::Rope,
        new_text: lapce_xi_rope::Rope,
        change: Arc<
            Mutex<(
                Option<TextDocumentContentChangeEvent>,
                Option<TextDocumentContentChangeEvent>,
            )>,
        >,
    ) {
        self.host.handle_did_change_text_document(
            language_id,
            document,
            delta,
            text,
            new_text,
            change,
        );
    }

    fn format_semantic_tokens(
        &self,
        tokens: SemanticTokens,
        text: Rope,
        f: Box<dyn RpcCallback<Vec<LineStyle>, RpcError>>,
    ) {
        self.host.format_semantic_tokens(tokens, text, f);
    }
}

impl LspClient {
    #[allow(clippy::too_many_arguments)]
    fn new(
        plugin_rpc: PluginCatalogRpcHandler,
        document_selector: DocumentSelector,
        workspace: Option<PathBuf>,
        volt_id: VoltID,
        volt_display_name: String,
        spawned_by: Option<PluginId>,
        plugin_id: Option<PluginId>,
        pwd: Option<PathBuf>,
        server_uri: Url,
        args: Vec<String>,
        options: Option<Value>,
        env: Arc<HashMap<String, String>>,
    ) -> Result<Self> {
        let server = match server_uri.scheme() {
            "file" => {
                let path = server_uri.to_file_path().map_err(|_| anyhow!(""))?;
                #[cfg(unix)]
                if let Err(err) = std::process::Command::new("chmod")
                    .arg("+x")
                    .arg(&path)
                    .output()
                {
                    tracing::error!("{:?}", err);
                }
                path.to_str().ok_or_else(|| anyhow!(""))?.to_string()
            }
            "urn" => server_uri.path().to_string(),
            _ => return Err(anyhow!("uri not supported")),
        };

        let mut process = Self::process(workspace.as_ref(), &server, &args, &env)?;
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let stderr = process.stderr.take().unwrap();

        let mut writer = Box::new(BufWriter::new(stdin));
        let (io_tx, io_rx) = crossbeam_channel::unbounded();
        let server_rpc = PluginServerRpcHandler::new(
            volt_id.clone(),
            spawned_by,
            plugin_id,
            io_tx.clone(),
        );
        // Writer thread: serializes JSON-RPC messages with Content-Length headers
        // and writes them to the LSP's stdin. Exits on Shutdown to allow the
        // process to terminate cleanly.
        thread::spawn(move || {
            for msg in io_rx {
                if msg
                    .get_method()
                    .map(|x| x == lsp_types::request::Shutdown::METHOD)
                    .unwrap_or_default()
                {
                    break;
                }
                if let Ok(msg) = serde_json::to_string(&msg) {
                    tracing::debug!("write to lsp: {}", msg);
                    let msg =
                        format!("Content-Length: {}\r\n\r\n{}", msg.len(), msg);
                    if let Err(err) = writer.write(msg.as_bytes()) {
                        tracing::error!("{:?}", err);
                    }
                    if let Err(err) = writer.flush() {
                        tracing::error!("{:?}", err);
                    }
                }
            }
        });

        // Reader thread: continuously reads Content-Length framed messages from
        // the LSP's stdout and dispatches them through handle_plugin_server_message.
        // If a response message needs to be sent back (e.g., for host requests),
        // it's forwarded to the writer via io_tx.
        let local_server_rpc = server_rpc.clone();
        let core_rpc = plugin_rpc.core_rpc.clone();
        let volt_id_closure = volt_id.clone();
        let name = volt_display_name.clone();
        thread::spawn(move || {
            let mut reader = Box::new(BufReader::new(stdout));
            loop {
                match read_message(&mut reader) {
                    Ok(message_str) => {
                        if !message_str.contains("$/progress") {
                            tracing::debug!("read from lsp: {}", message_str);
                        }
                        if let Some(resp) = handle_plugin_server_message(
                            &local_server_rpc,
                            &message_str,
                            &name,
                        ) {
                            if let Err(err) = io_tx.send(resp) {
                                tracing::error!("{:?}", err);
                            }
                        }
                    }
                    Err(_err) => {
                        core_rpc.log(
                            lapce_rpc::core::LogLevel::Error,
                            format!("lsp server {server} stopped!"),
                            Some(format!(
                                "lapce_proxy::plugin::lsp::{}::{}::stopped",
                                volt_id_closure.author, volt_id_closure.name
                            )),
                        );
                        return;
                    }
                };
            }
        });

        let core_rpc = plugin_rpc.core_rpc.clone();
        let volt_id_closure = volt_id.clone();
        thread::spawn(move || {
            let mut reader = Box::new(BufReader::new(stderr));
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(n) => {
                        if n == 0 {
                            return;
                        }
                        core_rpc.log(
                            lapce_rpc::core::LogLevel::Trace,
                            line.trim_end().to_string(),
                            Some(format!(
                                "lapce_proxy::plugin::lsp::{}::{}::stderr",
                                volt_id_closure.author, volt_id_closure.name
                            )),
                        );
                    }
                    Err(_) => {
                        return;
                    }
                }
            }
        });

        let host = PluginHostHandler::new(
            workspace.clone(),
            pwd,
            volt_id,
            volt_display_name,
            document_selector,
            plugin_rpc.core_rpc.clone(),
            server_rpc.clone(),
            plugin_rpc.clone(),
        );

        Ok(Self {
            plugin_rpc,
            server_rpc,
            process,
            workspace,
            host,
            options,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        plugin_rpc: PluginCatalogRpcHandler,
        document_selector: DocumentSelector,
        workspace: Option<PathBuf>,
        volt_id: VoltID,
        volt_display_name: String,
        spawned_by: Option<PluginId>,
        plugin_id: Option<PluginId>,
        pwd: Option<PathBuf>,
        server_uri: Url,
        args: Vec<String>,
        options: Option<Value>,
        env: Arc<HashMap<String, String>>,
    ) -> Result<PluginId> {
        let mut lsp = Self::new(
            plugin_rpc,
            document_selector,
            workspace,
            volt_id,
            volt_display_name,
            spawned_by,
            plugin_id,
            pwd,
            server_uri,
            args,
            options,
            env,
        )?;
        let plugin_id = lsp.server_rpc.plugin_id;

        let rpc = lsp.server_rpc.clone();
        thread::spawn(move || {
            rpc.mainloop(&mut lsp);
        });
        Ok(plugin_id)
    }

    fn initialize(&mut self) {
        let root_uri = self
            .workspace
            .clone()
            .map(|p| Url::from_directory_path(p).unwrap());
        tracing::debug!("initialization_options {:?}", self.options);
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(process::id()),
            root_uri: root_uri.clone(),
            initialization_options: self.options.clone(),
            capabilities: client_capabilities(),
            trace: Some(TraceValue::Verbose),
            workspace_folders: root_uri.map(|uri| {
                vec![WorkspaceFolder {
                    name: uri.as_str().to_string(),
                    uri,
                }]
            }),
            client_info: Some(ClientInfo {
                name: meta::NAME.to_owned(),
                version: Some(meta::VERSION.to_owned()),
            }),
            locale: None,
            root_path: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        match self.server_rpc.server_request(
            Initialize::METHOD,
            params,
            None,
            None,
            false,
        ) {
            Ok(value) => {
                let result: InitializeResult =
                    serde_json::from_value(value).unwrap();
                self.host.server_capabilities = result.capabilities;
                self.server_rpc.server_notification(
                    Initialized::METHOD,
                    InitializedParams {},
                    None,
                    None,
                    false,
                );
                if self
                    .plugin_rpc
                    .plugin_server_loaded(self.server_rpc.clone())
                    .is_err()
                {
                    self.server_rpc.shutdown();
                    self.shutdown();
                }
            }
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
        //     move |result| {
        //         if let Ok(value) = result {
        //             let result: InitializeResult =
        //                 serde_json::from_value(value).unwrap();
        //             server_rpc.handle_rpc(PluginServerRpc::Handler(
        //                 PluginHandlerNotification::InitializeDone(result),
        //             ));
        //         }
        //     },
        // );
    }

    fn shutdown(&mut self) {
        if let Err(err) = self.process.kill() {
            tracing::error!("{:?}", err);
        }
        if let Err(err) = self.process.wait() {
            tracing::error!("{:?}", err);
        }
    }

    /// Spawns the language server child process. The resolved shell environment
    /// is passed in to ensure version managers (mise, asdf, nvm, etc.) are active
    /// and the correct tool versions are found.
    fn process(
        workspace: Option<&PathBuf>,
        server: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Child> {
        let mut process = Command::new(server);
        if let Some(workspace) = workspace {
            process.current_dir(workspace);
        }

        process.args(args);
        if !env.is_empty() {
            process.envs(env);
        }

        // CREATE_NO_WINDOW (0x08000000) prevents a console window from flashing
        // on Windows when spawning the language server.
        #[cfg(target_os = "windows")]
        let process = process.creation_flags(0x08000000);
        let child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(child)
    }
}

pub struct DocumentFilter {
    /// The document must have this language id, if it exists
    pub language_id: Option<String>,
    /// The document's path must match this glob, if it exists
    pub pattern: Option<globset::GlobMatcher>,
    // TODO: URI Scheme from lsp-types document filter
}
impl DocumentFilter {
    /// Constructs a document filter from the LSP version
    /// This ignores any fields that are badly constructed
    pub(crate) fn from_lsp_filter_loose(
        filter: &lsp_types::DocumentFilter,
    ) -> DocumentFilter {
        DocumentFilter {
            language_id: filter.language.clone(),
            // TODO: clean this up
            pattern: filter
                .pattern
                .as_deref()
                .map(globset::Glob::new)
                .and_then(Result::ok)
                .map(|x| globset::Glob::compile_matcher(&x)),
        }
    }
}

pub enum LspHeader {
    ContentType,
    ContentLength(usize),
}

fn parse_header(s: &str) -> Result<LspHeader> {
    let split: Vec<String> =
        s.splitn(2, ": ").map(|s| s.trim().to_lowercase()).collect();
    if split.len() != 2 {
        return Err(anyhow!("Malformed"));
    };
    match split[0].as_ref() {
        HEADER_CONTENT_TYPE => Ok(LspHeader::ContentType),
        HEADER_CONTENT_LENGTH => {
            Ok(LspHeader::ContentLength(split[1].parse::<usize>()?))
        }
        _ => Err(anyhow!("Unknown parse error occurred")),
    }
}

pub fn read_message<T: BufRead>(reader: &mut T) -> Result<String> {
    let mut buffer = String::new();
    let mut content_length: Option<usize> = None;

    loop {
        buffer.clear();
        let _ = reader.read_line(&mut buffer)?;
        // eprin
        match &buffer {
            s if s.trim().is_empty() => break,
            s => {
                match parse_header(s)? {
                    LspHeader::ContentLength(len) => content_length = Some(len),
                    LspHeader::ContentType => (),
                };
            }
        };
    }

    let content_length = content_length
        .ok_or_else(|| anyhow!("missing content-length header: {}", buffer))?;

    let mut body_buffer = vec![0; content_length];
    reader.read_exact(&mut body_buffer)?;

    let body = String::from_utf8(body_buffer)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── parse_header ──

    #[test]
    fn parse_header_content_length() {
        match parse_header("Content-Length: 42").unwrap() {
            LspHeader::ContentLength(len) => assert_eq!(len, 42),
            LspHeader::ContentType => panic!("expected ContentLength"),
        }
    }

    #[test]
    fn parse_header_content_length_case_insensitive() {
        match parse_header("CONTENT-LENGTH: 100").unwrap() {
            LspHeader::ContentLength(len) => assert_eq!(len, 100),
            LspHeader::ContentType => panic!("expected ContentLength"),
        }
    }

    #[test]
    fn parse_header_content_type() {
        match parse_header("Content-Type: application/json").unwrap() {
            LspHeader::ContentType => {} // expected
            LspHeader::ContentLength(_) => panic!("expected ContentType"),
        }
    }

    #[test]
    fn parse_header_content_type_case_insensitive() {
        match parse_header("content-type: utf-8").unwrap() {
            LspHeader::ContentType => {} // expected
            LspHeader::ContentLength(_) => panic!("expected ContentType"),
        }
    }

    #[test]
    fn parse_header_malformed_no_colon() {
        assert!(parse_header("malformed").is_err());
    }

    #[test]
    fn parse_header_unknown_header() {
        assert!(parse_header("X-Custom: value").is_err());
    }

    #[test]
    fn parse_header_empty_string() {
        assert!(parse_header("").is_err());
    }

    #[test]
    fn parse_header_invalid_content_length() {
        assert!(parse_header("Content-Length: abc").is_err());
    }

    #[test]
    fn parse_header_whitespace_trimmed() {
        match parse_header("  Content-Length  :  256  ").unwrap() {
            LspHeader::ContentLength(len) => assert_eq!(len, 256),
            LspHeader::ContentType => panic!("expected ContentLength"),
        }
    }

    // ── read_message ──

    #[test]
    fn read_message_simple() {
        let body = r#"{"jsonrpc":"2.0"}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_with_content_type_header() {
        let body = r#"{"id":1}"#;
        let msg = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_missing_content_length() {
        let msg = "Content-Type: application/json\r\n\r\n{}";
        let mut reader = Cursor::new(msg.as_bytes().to_vec());
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn read_message_empty_input() {
        let mut reader = Cursor::new(Vec::new());
        // Empty input has no content-length header
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn read_message_large_body() {
        let body = "a".repeat(1000);
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_unicode_body() {
        let body = r#"{"result":"こんにちは"}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }

    #[test]
    fn read_message_content_length_zero() {
        let msg = "Content-Length: 0\r\n\r\n";
        let mut reader = Cursor::new(msg.as_bytes().to_vec());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn read_message_headers_case_insensitive() {
        let body = "hello";
        let msg = format!("CONTENT-LENGTH: {}\r\n\r\n{}", body.len(), body);
        let mut reader = Cursor::new(msg.into_bytes());
        let result = read_message(&mut reader).unwrap();
        assert_eq!(result, body);
    }
}
