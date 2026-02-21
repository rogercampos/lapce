use std::sync::{Arc, atomic::AtomicBool, mpsc::Sender};

/// Watches for config file changes (create/modify/remove) and debounces
/// notifications. Uses an AtomicBool as a simple lock so that rapid successive
/// file changes (e.g. from an editor doing write-rename) only trigger one
/// config reload after a 500ms quiet period.
pub struct ConfigWatcher {
    tx: Sender<()>,
    delay_handler: Arc<AtomicBool>,
}

impl notify::EventHandler for ConfigWatcher {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    // Use compare_exchange as a mutex: only the first event in a
                    // burst spawns the delay thread; subsequent events are dropped.
                    if self
                        .delay_handler
                        .compare_exchange(
                            false,
                            true,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        let config_mutex = self.delay_handler.clone();
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(
                                500,
                            ));
                            // Reset the flag unconditionally before sending, so that
                            // even if tx.send() fails (receiver dropped), the watcher
                            // is not permanently disabled.
                            config_mutex
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                            if let Err(err) = tx.send(()) {
                                tracing::error!("{:?}", err);
                            }
                        });
                    }
                }
                _ => {}
            },
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
    }
}

impl ConfigWatcher {
    pub fn new(tx: Sender<()>) -> Self {
        Self {
            tx,
            delay_handler: Arc::new(AtomicBool::new(false)),
        }
    }
}
