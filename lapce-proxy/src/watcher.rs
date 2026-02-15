use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crossbeam_channel::{Receiver, unbounded};
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
    recommended_watcher,
};
use parking_lot::Mutex;

/// Wrapper around a `notify::Watcher`. It runs the inner watcher
/// in a separate thread, and communicates with it via a [crossbeam channel].
/// [crossbeam channel]: https://docs.rs/crossbeam-channel
pub struct FileWatcher {
    rx_event: Option<Receiver<Result<Event, notify::Error>>>,
    inner: RecommendedWatcher,
    state: Arc<Mutex<WatcherState>>,
}

#[derive(Debug, Default)]
struct WatcherState {
    watchees: Vec<Watchee>,
}

/// Tracks a registered 'that-which-is-watched'.
#[doc(hidden)]
struct Watchee {
    path: PathBuf,
    recursive: bool,
    token: WatchToken,
    filter: Option<Box<PathFilter>>,
}

/// Token provided to `FileWatcher`, to associate events with
/// interested parties.
///
/// Note: `WatchToken`s are assumed to correspond with an
/// 'area of interest'; that is, they are used to route delivery
/// of events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchToken(pub usize);

/// A trait for types which can be notified of new events.
/// New events are accessible through the `FileWatcher` instance.
pub trait Notify: Send {
    fn notify(&self, events: Vec<(WatchToken, Event)>);
}

pub type PathFilter = dyn Fn(&Path) -> bool + Send + 'static;

impl FileWatcher {
    pub fn new() -> Self {
        let (tx_event, rx_event) = unbounded();

        let state = Arc::new(Mutex::new(WatcherState::default()));

        let inner = recommended_watcher(tx_event).expect("watcher should spawn");

        FileWatcher {
            rx_event: Some(rx_event),
            inner,
            state,
        }
    }

    /// Starts the event processing loop on a background thread. Each raw notify
    /// event is matched against all registered watchees to determine which tokens
    /// (subscribers) care about it. The event may be delivered to multiple tokens
    /// if paths overlap (e.g., a file is watched individually AND as part of a
    /// recursive workspace watch).
    ///
    /// Can only be called once -- the receiver is consumed via `take()`.
    pub fn notify<T: Notify + 'static>(&mut self, peer: T) {
        let rx_event = self.rx_event.take().unwrap();
        let state = self.state.clone();
        std::thread::spawn(move || {
            while let Ok(Ok(event)) = rx_event.recv() {
                let mut events = Vec::new();
                {
                    let mut state = state.lock();
                    let WatcherState {
                        ref mut watchees, ..
                    } = *state;

                    watchees
                        .iter()
                        .filter(|w| w.wants_event(&event))
                        .map(|w| w.token)
                        .for_each(|t| events.push((t, event.clone())));
                }

                peer.notify(events);
            }
        });
    }

    /// Begin watching `path`. As `Event`s (documented in the
    /// [notify](https://docs.rs/notify) crate) arrive, they are stored
    /// with the associated `token` and a task is added to the runloop's
    /// idle queue.
    ///
    /// Delivery of events then requires that the runloop's handler
    /// correctly forward the `handle_idle` call to the interested party.
    pub fn watch(&mut self, path: &Path, recursive: bool, token: WatchToken) {
        self.watch_impl(path, recursive, token, None);
    }

    /// Like `watch`, but taking a predicate function that filters delivery
    /// of events based on their path.
    pub fn watch_filtered<F>(
        &mut self,
        path: &Path,
        recursive: bool,
        token: WatchToken,
        filter: F,
    ) where
        F: Fn(&Path) -> bool + Send + 'static,
    {
        let filter = Box::new(filter) as Box<PathFilter>;
        self.watch_impl(path, recursive, token, Some(filter));
    }

    fn watch_impl(
        &mut self,
        path: &Path,
        recursive: bool,
        token: WatchToken,
        filter: Option<Box<PathFilter>>,
    ) {
        // Canonicalize to avoid duplicate watches on the same physical path
        // reached via different symlink routes. Silently skip if path doesn't exist
        // (e.g., watching a file that hasn't been created yet).
        let path = match path.canonicalize() {
            Ok(ref p) => p.to_owned(),
            Err(_) => {
                return;
            }
        };

        let mut state = self.state.lock();

        let w = Watchee {
            path,
            recursive,
            token,
            filter,
        };
        let mode = mode_from_bool(w.recursive);

        // Only register with the OS watcher if no other watchee already covers
        // this exact path. Multiple tokens can share one OS watch.
        if !state.watchees.iter().any(|w2| w.path == w2.path) {
            if let Err(err) = self.inner.watch(&w.path, mode) {
                tracing::error!("{:?}", err);
            }
        }

        state.watchees.push(w);
    }

    /// Removes the provided token/path pair from the watch list.
    /// Does not stop watching this path, if it is associated with
    /// other tokens.
    pub fn unwatch(&mut self, path: &Path, token: WatchToken) {
        let mut state = self.state.lock();

        let idx = state
            .watchees
            .iter()
            .position(|w| w.token == token && w.path == path);

        if let Some(idx) = idx {
            let removed = state.watchees.remove(idx);
            if !state.watchees.iter().any(|w| w.path == removed.path) {
                if let Err(err) = self.inner.unwatch(&removed.path) {
                    tracing::error!("{:?}", err);
                }
            }
            //TODO: Ideally we would be tracking what paths we're watching with
            // some prefix-tree-like structure, which would let us keep track
            // of when some child path might need to be reregistered. How this
            // works and when registration would be required is dependent on
            // the underlying notification mechanism, however. There's an
            // in-progress rewrite of the Notify crate which use under the
            // hood, and a component of that rewrite is adding this
            // functionality; so until that lands we're using a fairly coarse
            // heuristic to determine if we need to re-watch subpaths.

            // if this was recursive, check if any child paths need to be
            // manually re-added
            if removed.recursive {
                // do this in two steps because we've borrowed mutably up top
                let to_add = state
                    .watchees
                    .iter()
                    .filter(|w| w.path.starts_with(&removed.path))
                    .map(|w| (w.path.to_owned(), mode_from_bool(w.recursive)))
                    .collect::<Vec<_>>();

                for (path, mode) in to_add {
                    if let Err(err) = self.inner.watch(&path, mode) {
                        tracing::error!("{:?}", err);
                    }
                }
            }
        }
    }
}

impl Default for FileWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Watchee {
    /// Determines whether this watchee is interested in the given event.
    /// Rename(Both) events carry two paths (source and destination) and we
    /// match against either, since the user cares about both sides of a rename.
    fn wants_event(&self, event: &Event) -> bool {
        match &event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                if event.paths.len() == 2 {
                    self.applies_to_path(&event.paths[0])
                        || self.applies_to_path(&event.paths[1])
                } else {
                    false
                }
            }
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) => {
                if event.paths.len() == 1 {
                    self.applies_to_path(&event.paths[0])
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Checks whether a given event path falls within this watchee's scope.
    /// For non-recursive watches, we match the exact path OR direct children
    /// (one level deep), because the `notify` crate delivers events for a
    /// watched directory's immediate contents even in NonRecursive mode.
    fn applies_to_path(&self, path: &Path) -> bool {
        let general_case = if path.starts_with(&self.path) {
            (self.recursive || self.path == path)
                || path.parent() == Some(self.path.as_path())
        } else {
            false
        };

        if let Some(ref filter) = self.filter {
            general_case && filter(path)
        } else {
            general_case
        }
    }
}
impl std::fmt::Debug for Watchee {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Watchee path: {:?}, r {}, t {} f {}",
            self.path,
            self.recursive,
            self.token.0,
            self.filter.is_some()
        )
    }
}

fn mode_from_bool(is_recursive: bool) -> RecursiveMode {
    if is_recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};

    fn watchee(path: &str, recursive: bool) -> Watchee {
        Watchee {
            path: PathBuf::from(path),
            recursive,
            token: WatchToken(0),
            filter: None,
        }
    }

    fn watchee_filtered<F>(path: &str, recursive: bool, filter: F) -> Watchee
    where
        F: Fn(&Path) -> bool + Send + 'static,
    {
        Watchee {
            path: PathBuf::from(path),
            recursive,
            token: WatchToken(0),
            filter: Some(Box::new(filter)),
        }
    }

    // --- applies_to_path tests ---

    #[test]
    fn applies_to_path_exact_match() {
        let w = watchee("/home/user/file.txt", false);
        assert!(w.applies_to_path(Path::new("/home/user/file.txt")));
    }

    #[test]
    fn applies_to_path_direct_child_non_recursive() {
        // Non-recursive watch on a directory matches immediate children
        let w = watchee("/home/user/project", false);
        assert!(w.applies_to_path(Path::new("/home/user/project/file.txt")));
    }

    #[test]
    fn applies_to_path_grandchild_non_recursive_rejected() {
        // Non-recursive watch should NOT match grandchildren
        let w = watchee("/home/user/project", false);
        assert!(!w.applies_to_path(Path::new("/home/user/project/src/main.rs")));
    }

    #[test]
    fn applies_to_path_grandchild_recursive() {
        // Recursive watch should match nested descendants
        let w = watchee("/home/user/project", true);
        assert!(w.applies_to_path(Path::new("/home/user/project/src/main.rs")));
        assert!(w.applies_to_path(Path::new(
            "/home/user/project/src/deep/nested/file.rs"
        )));
    }

    #[test]
    fn applies_to_path_unrelated_path() {
        let w = watchee("/home/user/project", true);
        assert!(!w.applies_to_path(Path::new("/home/other/file.txt")));
    }

    #[test]
    fn applies_to_path_sibling_not_matched() {
        let w = watchee("/home/user/project", false);
        assert!(!w.applies_to_path(Path::new("/home/user/other_project/file.txt")));
    }

    #[test]
    fn applies_to_path_with_filter_passing() {
        let w = watchee_filtered("/home/user/project", true, |p: &Path| {
            p.extension().and_then(|e| e.to_str()) == Some("rs")
        });
        assert!(w.applies_to_path(Path::new("/home/user/project/src/main.rs")));
    }

    #[test]
    fn applies_to_path_with_filter_rejecting() {
        let w = watchee_filtered("/home/user/project", true, |p: &Path| {
            p.extension().and_then(|e| e.to_str()) == Some("rs")
        });
        assert!(!w.applies_to_path(Path::new("/home/user/project/src/main.js")));
    }

    #[test]
    fn applies_to_path_filter_only_applied_when_general_passes() {
        // Filter should not be reached for paths outside the watched scope
        let w = watchee_filtered("/home/user/project", false, |_: &Path| true);
        assert!(!w.applies_to_path(Path::new("/other/path/file.txt")));
    }

    // --- wants_event tests ---

    fn create_event(kind: EventKind, paths: Vec<PathBuf>) -> Event {
        Event {
            kind,
            paths,
            attrs: Default::default(),
        }
    }

    #[test]
    fn wants_event_modify_single_path_match() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            vec![PathBuf::from("/home/user/project/src/main.rs")],
        );
        assert!(w.wants_event(&event));
    }

    #[test]
    fn wants_event_modify_single_path_no_match() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            vec![PathBuf::from("/other/path/file.txt")],
        );
        assert!(!w.wants_event(&event));
    }

    #[test]
    fn wants_event_create() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Create(CreateKind::File),
            vec![PathBuf::from("/home/user/project/new_file.rs")],
        );
        assert!(w.wants_event(&event));
    }

    #[test]
    fn wants_event_remove() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Remove(RemoveKind::File),
            vec![PathBuf::from("/home/user/project/old_file.rs")],
        );
        assert!(w.wants_event(&event));
    }

    #[test]
    fn wants_event_rename_both_matches_source() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![
                PathBuf::from("/home/user/project/old_name.rs"),
                PathBuf::from("/other/place/new_name.rs"),
            ],
        );
        assert!(w.wants_event(&event));
    }

    #[test]
    fn wants_event_rename_both_matches_destination() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![
                PathBuf::from("/other/place/old_name.rs"),
                PathBuf::from("/home/user/project/new_name.rs"),
            ],
        );
        assert!(w.wants_event(&event));
    }

    #[test]
    fn wants_event_rename_both_neither_matches() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![
                PathBuf::from("/other/old.rs"),
                PathBuf::from("/other/new.rs"),
            ],
        );
        assert!(!w.wants_event(&event));
    }

    #[test]
    fn wants_event_access_kind_ignored() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Access(notify::event::AccessKind::Read),
            vec![PathBuf::from("/home/user/project/file.rs")],
        );
        assert!(!w.wants_event(&event));
    }

    #[test]
    fn wants_event_other_kind_ignored() {
        let w = watchee("/home/user/project", true);
        let event = create_event(
            EventKind::Other,
            vec![PathBuf::from("/home/user/project/file.rs")],
        );
        assert!(!w.wants_event(&event));
    }

    #[test]
    fn wants_event_modify_wrong_path_count_ignored() {
        let w = watchee("/home/user/project", true);
        // A Modify event with 0 paths should not match
        let event = create_event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            vec![],
        );
        assert!(!w.wants_event(&event));

        // A non-rename Modify event with 2 paths should not match
        let event = create_event(
            EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            vec![
                PathBuf::from("/home/user/project/a.rs"),
                PathBuf::from("/home/user/project/b.rs"),
            ],
        );
        assert!(!w.wants_event(&event));
    }

    #[test]
    fn wants_event_rename_both_wrong_path_count() {
        let w = watchee("/home/user/project", true);
        // Rename(Both) with only 1 path should not match
        let event = create_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            vec![PathBuf::from("/home/user/project/file.rs")],
        );
        assert!(!w.wants_event(&event));
    }

    // --- mode_from_bool tests ---

    #[test]
    fn mode_from_bool_recursive() {
        assert_eq!(mode_from_bool(true), RecursiveMode::Recursive);
    }

    #[test]
    fn mode_from_bool_non_recursive() {
        assert_eq!(mode_from_bool(false), RecursiveMode::NonRecursive);
    }
}
