use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, mpsc::Sender},
};

use floem::{ext_event::create_signal_from_channel, reactive::ReadSignal};
use lapce_proxy::dispatch::Dispatcher;
use lapce_rpc::{
    core::{CoreHandler, CoreNotification, CoreRpcHandler},
    plugin::VoltID,
    proxy::ProxyRpcHandler,
};

use crate::workspace::LapceWorkspace;

pub struct Proxy {
    pub tx: Sender<CoreNotification>,
}

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

pub fn new_proxy(
    workspace: Arc<LapceWorkspace>,
    disabled_volts: Vec<VoltID>,
    extra_plugin_paths: Vec<PathBuf>,
    plugin_configurations: HashMap<String, HashMap<String, serde_json::Value>>,
) -> ProxyData {
    let proxy_rpc = ProxyRpcHandler::new();
    let core_rpc = CoreRpcHandler::new();

    {
        let core_rpc = core_rpc.clone();
        let proxy_rpc = proxy_rpc.clone();
        std::thread::Builder::new()
            .name("ProxyRpcHandler".to_owned())
            .spawn(move || {
                proxy_rpc.initialize(
                    workspace.path.clone(),
                    disabled_volts,
                    extra_plugin_paths,
                    plugin_configurations,
                    1,
                    1,
                );

                let core_rpc = core_rpc.clone();
                let proxy_rpc = proxy_rpc.clone();
                let mut dispatcher = Dispatcher::new(core_rpc, proxy_rpc);
                let proxy_rpc = dispatcher.proxy_rpc.clone();
                proxy_rpc.mainloop(&mut dispatcher);
            })
            .unwrap();
    }

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

pub fn new_command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    use std::os::windows::process::CommandExt;
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000);
    cmd
}
