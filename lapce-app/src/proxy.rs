use std::{process::Command, sync::Arc};

use floem::{ext_event::create_signal_from_channel, reactive::ReadSignal};
use lapce_proxy::dispatch::Dispatcher;
use lapce_rpc::{
    core::{CoreHandler, CoreNotification, CoreRpcHandler},
    proxy::ProxyRpcHandler,
};

use crate::workspace::LapceWorkspace;

/// The app-side handler for core notifications from the proxy.
/// Simply forwards notifications through the mpsc channel to be picked up
/// by `create_signal_from_channel` and processed in the reactive system.
pub struct Proxy {
    pub tx: std::sync::mpsc::Sender<CoreNotification>,
}

/// Holds both ends of the proxy communication bridge:
/// - `proxy_rpc`: sends requests TO the proxy (open file, search, LSP operations)
/// - `core_rpc`: receives notifications FROM the proxy (diagnostics, completions, etc.)
/// - `notification`: a reactive signal that emits each incoming CoreNotification
#[derive(Clone)]
pub struct ProxyData {
    pub proxy_rpc: ProxyRpcHandler,
    pub core_rpc: CoreRpcHandler,
    pub notification: ReadSignal<Option<CoreNotification>>,
}

impl ProxyData {
    pub fn shutdown(&self) {
        self.proxy_rpc.shutdown();
        self.core_rpc.shutdown();
    }
}

/// Spawns the proxy backend and sets up the two-way communication bridge.
///
/// Two dedicated threads are created:
/// 1. **ProxyRpcHandler**: runs the Dispatcher mainloop, processing requests from the
///    app (file open, search, LSP) and sending responses/notifications back via core_rpc.
/// 2. **CoreRpcHandler**: reads notifications from core_rpc and forwards them through
///    an mpsc channel that gets bridged to a Floem reactive signal.
///
/// For local workspaces, the proxy runs in-process (as threads, not a separate process).
/// The `proxy_rpc.initialize()` call triggers LSP server startup.
pub fn new_proxy(workspace: Arc<LapceWorkspace>) -> ProxyData {
    let proxy_rpc = ProxyRpcHandler::new();
    let core_rpc = CoreRpcHandler::new();

    // Thread 1: Proxy dispatcher - processes app requests and runs LSP logic
    {
        let core_rpc = core_rpc.clone();
        let proxy_rpc = proxy_rpc.clone();
        std::thread::Builder::new()
            .name("ProxyRpcHandler".to_owned())
            .spawn(move || {
                tracing::info!(
                    "[proxy] ProxyRpcHandler thread started, workspace={:?}",
                    workspace.path
                );
                proxy_rpc.initialize(workspace.path.clone(), 1, 1);
                tracing::info!(
                    "[proxy] Initialize notification queued, creating dispatcher"
                );

                let core_rpc = core_rpc.clone();
                let proxy_rpc = proxy_rpc.clone();
                let mut dispatcher = Dispatcher::new(core_rpc, proxy_rpc);
                let proxy_rpc = dispatcher.proxy_rpc.clone();
                tracing::info!("[proxy] Starting dispatcher mainloop");
                proxy_rpc.mainloop(&mut dispatcher);
                tracing::info!("[proxy] Dispatcher mainloop exited");
            })
            .unwrap();
    }

    // Thread 2: Core notification reader - bridges proxy notifications to the UI thread
    // via an mpsc channel that Floem's create_signal_from_channel converts to a signal.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let core_rpc = core_rpc.clone();
        std::thread::Builder::new()
            .name("CoreRpcHandler".to_owned())
            .spawn(move || {
                let mut proxy = Proxy { tx };
                core_rpc.mainloop(&mut proxy);
            })
            .unwrap()
    };

    // Convert the mpsc receiver into a Floem reactive signal. Each time a notification
    // is sent, the signal updates, triggering the effect in WorkspaceData::new that
    // calls handle_core_notification().
    let notification = create_signal_from_channel(rx);

    ProxyData {
        proxy_rpc,
        core_rpc,
        notification,
    }
}

impl CoreHandler for Proxy {
    fn handle_notification(&mut self, rpc: lapce_rpc::core::CoreNotification) {
        if let Err(err) = self.tx.send(rpc) {
            tracing::error!("{:?}", err);
        }
    }

    fn handle_request(
        &mut self,
        _id: lapce_rpc::RequestId,
        _rpc: lapce_rpc::core::CoreRequest,
    ) {
    }
}

/// Creates a Command with platform-specific flags. On Windows, sets
/// CREATE_NO_WINDOW (0x08000000) to prevent console windows from flashing.
pub fn new_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);
    cmd
}
