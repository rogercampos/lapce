use std::{
    cmp::{Ord, Ordering, PartialOrd},
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// UTF8 line and column-offset
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct LineCol {
    pub line: usize,
    pub column: usize,
}

#[derive(
    Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct PathObject {
    pub path: PathBuf,
    pub linecol: Option<LineCol>,
    pub is_dir: bool,
}

impl PathObject {
    pub fn new(
        path: PathBuf,
        is_dir: bool,
        line: usize,
        column: usize,
    ) -> PathObject {
        PathObject {
            path,
            is_dir,
            linecol: Some(LineCol { line, column }),
        }
    }

    pub fn from_path(path: PathBuf, is_dir: bool) -> PathObject {
        PathObject {
            path,
            is_dir,
            linecol: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileNodeViewKind {
    /// An actual file/directory
    Path(PathBuf),
    /// We are renaming the file at this path
    Renaming { path: PathBuf, err: Option<String> },
    /// We are naming a new file/directory
    Naming { err: Option<String> },
    Duplicating {
        /// The path that is being duplicated
        source: PathBuf,
        err: Option<String>,
    },
}
impl FileNodeViewKind {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Path(path) => Some(path),
            Self::Renaming { path, .. } => Some(path),
            Self::Naming { .. } => None,
            Self::Duplicating { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamingState {
    /// Actively naming
    Naming,
    /// Application of the naming is pending
    Pending,
    /// There's an active error with the typed name
    Err { err: String },
}
impl NamingState {
    pub fn is_accepting_input(&self) -> bool {
        match self {
            Self::Naming | Self::Err { .. } => true,
            Self::Pending => false,
        }
    }

    pub fn is_err(&self) -> bool {
        match self {
            Self::Naming | Self::Pending => false,
            Self::Err { .. } => true,
        }
    }

    pub fn err(&self) -> Option<&str> {
        match self {
            Self::Err { err } => Some(err.as_str()),
            _ => None,
        }
    }

    pub fn set_ok(&mut self) {
        *self = Self::Naming;
    }

    pub fn set_pending(&mut self) {
        *self = Self::Pending;
    }

    pub fn set_err(&mut self, err: String) {
        *self = Self::Err { err };
    }
}

/// Stores the state of any in progress rename of a path.
///
/// The `editor_needs_reset` field is `true` if the rename editor should have its contents reset
/// when the view function next runs.
#[derive(Debug, Clone)]
pub struct Renaming {
    pub state: NamingState,
    /// Original file's path
    pub path: PathBuf,
    pub editor_needs_reset: bool,
}

#[derive(Debug, Clone)]
pub struct NewNode {
    pub state: NamingState,
    /// If true, then we are creating a directory
    pub is_dir: bool,
    /// The folder that the file/directory is being created within
    pub base_path: PathBuf,
    pub editor_needs_reset: bool,
}

#[derive(Debug, Clone)]
pub struct Duplicating {
    pub state: NamingState,
    /// Path to the item being duplicated
    pub path: PathBuf,
    pub editor_needs_reset: bool,
}

#[derive(Debug, Clone)]
pub enum Naming {
    None,
    Renaming(Renaming),
    NewNode(NewNode),
    Duplicating(Duplicating),
}
impl Naming {
    pub fn state(&self) -> Option<&NamingState> {
        match self {
            Self::None => None,
            Self::Renaming(rename) => Some(&rename.state),
            Self::NewNode(state) => Some(&state.state),
            Self::Duplicating(state) => Some(&state.state),
        }
    }

    pub fn state_mut(&mut self) -> Option<&mut NamingState> {
        match self {
            Self::None => None,
            Self::Renaming(rename) => Some(&mut rename.state),
            Self::NewNode(state) => Some(&mut state.state),
            Self::Duplicating(state) => Some(&mut state.state),
        }
    }

    pub fn is_accepting_input(&self) -> bool {
        self.state().is_some_and(NamingState::is_accepting_input)
    }

    pub fn editor_needs_reset(&self) -> bool {
        match self {
            Naming::None => false,
            Naming::Renaming(rename) => rename.editor_needs_reset,
            Naming::NewNode(state) => state.editor_needs_reset,
            Naming::Duplicating(state) => state.editor_needs_reset,
        }
    }

    pub fn set_editor_needs_reset(&mut self, needs_reset: bool) {
        match self {
            Naming::None => {}
            Naming::Renaming(rename) => rename.editor_needs_reset = needs_reset,
            Naming::NewNode(state) => state.editor_needs_reset = needs_reset,
            Naming::Duplicating(state) => state.editor_needs_reset = needs_reset,
        }
    }

    pub fn set_ok(&mut self) {
        if let Some(state) = self.state_mut() {
            state.set_ok();
        }
    }

    pub fn set_pending(&mut self) {
        if let Some(state) = self.state_mut() {
            state.set_pending();
        }
    }

    pub fn set_err(&mut self, err: String) {
        if let Some(state) = self.state_mut() {
            state.set_err(err);
        }
    }

    pub fn as_renaming(&self) -> Option<&Renaming> {
        match self {
            Naming::Renaming(rename) => Some(rename),
            _ => None,
        }
    }

    /// The extra node that should be added after the node at `path`
    pub fn extra_node(
        &self,
        is_dir: bool,
        level: usize,
        path: &Path,
    ) -> Option<FileNodeViewData> {
        match self {
            Naming::NewNode(n) if n.base_path == path => Some(FileNodeViewData {
                kind: FileNodeViewKind::Naming {
                    err: n.state.err().map(ToString::to_string),
                },
                is_dir: n.is_dir,
                is_root: false,
                open: false,
                level: level + 1,
            }),
            Naming::Duplicating(d) if d.path == path => Some(FileNodeViewData {
                kind: FileNodeViewKind::Duplicating {
                    source: d.path.to_path_buf(),
                    err: d.state.err().map(ToString::to_string),
                },
                is_dir,
                is_root: false,
                open: false,
                level: level + 1,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileNodeViewData {
    pub kind: FileNodeViewKind,
    pub is_dir: bool,
    pub is_root: bool,
    pub open: bool,
    pub level: usize,
}

/// A node in the file explorer tree. Represents a single file or directory.
/// Children are stored in a HashMap keyed by full path for O(1) lookups,
/// and sorted only when needed for display (via `sorted_children()`).
///
/// `children_open_count` is a cached count used for virtual list sizing --
/// it represents the total number of visible descendant nodes (including
/// recursively open subdirectories). This avoids walking the entire tree
/// on every frame to compute the list length.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileNodeItem {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Whether the directory's children have been read from disk.
    /// Directories start as `read: false` and are populated lazily on open.
    pub read: bool,
    /// Whether the directory is expanded in the explorer view.
    pub open: bool,
    pub children: HashMap<PathBuf, FileNodeItem>,
    /// Cached count of all visible descendant nodes. Updated via
    /// `update_node_count()` whenever the tree structure changes.
    pub children_open_count: usize,
}

impl PartialOrd for FileNodeItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Sorting: directories come before files, then alphabetical with human-sort
/// (so "file2" comes before "file10"). Uses `human_sort::compare` for
/// natural number ordering within filenames.
impl Ord for FileNodeItem {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_dir, other.is_dir) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => {
                let [self_file_name, other_file_name] = [&self.path, &other.path]
                    .map(|path| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase()
                    });
                human_sort::compare(&self_file_name, &other_file_name)
            }
        }
    }
}

impl FileNodeItem {
    /// Collect the children, sorted by name.
    /// Note: this will be empty if the directory has not been read.
    pub fn sorted_children(&self) -> Vec<&FileNodeItem> {
        let mut children = self.children.values().collect::<Vec<&FileNodeItem>>();
        children.sort();
        children
    }

    /// Collect the children, sorted by name.
    /// Note: this will be empty if the directory has not been read.
    pub fn sorted_children_mut(&mut self) -> Vec<&mut FileNodeItem> {
        let mut children = self
            .children
            .values_mut()
            .collect::<Vec<&mut FileNodeItem>>();
        children.sort();
        children
    }

    /// Returns an iterator over the ancestors of `path`, starting with the first descendant of `prefix`.
    ///
    /// # Example:
    /// (ignored because the function is private but I promise this passes)
    /// ```rust,ignore
    /// # use lapce_rpc::file::FileNodeItem;
    /// # use std::path::{Path, PathBuf};
    /// # use std::collections::HashMap;
    /// #
    /// let node_item = FileNodeItem {
    ///     path_buf: PathBuf::from("/pre/fix"),
    ///     // ...
    /// #    is_dir: true,
    /// #    read: false,
    /// #    open: false,
    /// #    children: HashMap::new(),
    /// #    children_open_count: 0,
    ///};
    /// let mut iter = node_item.ancestors_rev(Path::new("/pre/fix/foo/bar")).unwrap();
    /// assert_eq!(Some(Path::new("/pre/fix/foo")), iter.next());
    /// assert_eq!(Some(Path::new("/pre/fix/foo/bar")), iter.next());
    /// ```
    fn ancestors_rev<'a>(
        &self,
        path: &'a Path,
    ) -> Option<impl Iterator<Item = &'a Path> + use<'a>> {
        let take = if let Ok(suffix) = path.strip_prefix(&self.path) {
            suffix.components().count()
        } else {
            return None;
        };

        #[allow(clippy::needless_collect)] // Ancestors is not reversible
        let ancestors = path.ancestors().take(take).collect::<Vec<&Path>>();
        Some(ancestors.into_iter().rev())
    }

    /// Recursively get the node at `path`.
    pub fn get_file_node(&self, path: &Path) -> Option<&FileNodeItem> {
        self.ancestors_rev(path)?
            .try_fold(self, |node, path| node.children.get(path))
    }

    /// Recursively get the (mutable) node at `path`.
    pub fn get_file_node_mut(&mut self, path: &Path) -> Option<&mut FileNodeItem> {
        self.ancestors_rev(path)?
            .try_fold(self, |node, path| node.children.get_mut(path))
    }

    /// Remove a specific child from the node.
    /// The path is recursive and will remove the child from parent indicated by the path.
    pub fn remove_child(&mut self, path: &Path) -> Option<FileNodeItem> {
        let parent = path.parent()?;
        let node = self.get_file_node_mut(parent)?;
        let node = node.children.remove(path)?;
        for p in path.ancestors() {
            self.update_node_count(p);
        }

        Some(node)
    }

    /// Add a new (unread & unopened) child to the node.
    pub fn add_child(&mut self, path: &Path, is_dir: bool) -> Option<()> {
        let parent = path.parent()?;
        let node = self.get_file_node_mut(parent)?;
        node.children.insert(
            PathBuf::from(path),
            FileNodeItem {
                path: PathBuf::from(path),
                is_dir,
                read: false,
                open: false,
                children: HashMap::new(),
                children_open_count: 0,
            },
        );
        for p in path.ancestors() {
            self.update_node_count(p);
        }

        Some(())
    }

    /// Set the children of the node.
    /// Note: this opens the node.
    pub fn set_item_children(
        &mut self,
        path: &Path,
        children: HashMap<PathBuf, FileNodeItem>,
    ) {
        if let Some(node) = self.get_file_node_mut(path) {
            node.open = true;
            node.read = true;
            node.children = children;
        }

        for p in path.ancestors() {
            self.update_node_count(p);
        }
    }

    pub fn update_node_count_recursive(&mut self, path: &Path) {
        for current_path in path.ancestors() {
            self.update_node_count(current_path);
        }
    }

    pub fn update_node_count(&mut self, path: &Path) -> Option<()> {
        let node = self.get_file_node_mut(path)?;
        if node.is_dir {
            node.children_open_count = if node.open {
                node.children
                    .values()
                    .map(|item| item.children_open_count + 1)
                    .sum::<usize>()
            } else {
                0
            };
        }
        None
    }

    pub fn append_view_slice(
        &self,
        view_items: &mut Vec<FileNodeViewData>,
        naming: &Naming,
        min: usize,
        max: usize,
        current: usize,
        level: usize,
    ) -> usize {
        if current > max {
            return current;
        }
        if current + self.children_open_count < min {
            return current + self.children_open_count;
        }

        if current >= min {
            let kind = if let Naming::Renaming(r) = &naming {
                if r.path == self.path {
                    FileNodeViewKind::Renaming {
                        path: self.path.clone(),
                        err: r.state.err().map(ToString::to_string),
                    }
                } else {
                    FileNodeViewKind::Path(self.path.clone())
                }
            } else {
                FileNodeViewKind::Path(self.path.clone())
            };
            view_items.push(FileNodeViewData {
                kind,
                is_dir: self.is_dir,
                is_root: level == 1,
                open: self.open,
                level,
            });
        }

        self.append_children_view_slice(view_items, naming, min, max, current, level)
    }

    /// Calculate the row where the file resides
    pub fn find_file_at_line(&self, file_path: &Path) -> (bool, f64) {
        let mut line = 0.0;
        if !self.open {
            return (false, line);
        }
        for item in self.sorted_children() {
            line += 1.0;
            match (item.is_dir, item.open, item.path == file_path) {
                (_, _, true) => {
                    return (true, line);
                }
                (true, true, _) => {
                    let (found, item_position) = item.find_file_at_line(file_path);
                    line += item_position;
                    if found {
                        return (true, line);
                    }
                }
                _ => {}
            }
        }
        (false, line)
    }

    /// Helper for tests: create a leaf file node
    #[cfg(test)]
    fn new_file(path: impl Into<PathBuf>) -> Self {
        FileNodeItem {
            path: path.into(),
            is_dir: false,
            read: false,
            open: false,
            children: HashMap::new(),
            children_open_count: 0,
        }
    }

    /// Helper for tests: create a directory node
    #[cfg(test)]
    fn new_dir(path: impl Into<PathBuf>, open: bool) -> Self {
        FileNodeItem {
            path: path.into(),
            is_dir: true,
            read: true,
            open,
            children: HashMap::new(),
            children_open_count: 0,
        }
    }

    /// Append the children of this item with the given level
    pub fn append_children_view_slice(
        &self,
        view_items: &mut Vec<FileNodeViewData>,
        naming: &Naming,
        min: usize,
        max: usize,
        mut i: usize,
        level: usize,
    ) -> usize {
        let mut naming_extra = naming.extra_node(self.is_dir, level, &self.path);

        if !self.open {
            // If the folder isn't open, then we just put it right at the top
            if i >= min {
                if let Some(naming_extra) = naming_extra {
                    view_items.push(naming_extra);
                    i += 1;
                }
            }
            return i;
        }

        let naming_is_dir = naming_extra.as_ref().map(|n| n.is_dir).unwrap_or(false);
        // Immediately put the naming entry first if it's a directory
        if naming_is_dir {
            if let Some(node) = naming_extra.take() {
                // Actually add the node if it's within the range
                if i >= min {
                    view_items.push(node);
                    i += 1;
                }
            }
        }

        let mut after_dirs = false;

        for item in self.sorted_children() {
            // If we're naming a file at the root, then wait until we've added the directories
            // before adding the input node
            if naming_extra.is_some()
                && !naming_is_dir
                && !item.is_dir
                && !after_dirs
            {
                after_dirs = true;

                // If we're creating a new file node, then we show it after the directories
                // TODO(minor): should this be i >= min or i + 1 >= min?
                if i >= min {
                    if let Some(node) = naming_extra.take() {
                        view_items.push(node);
                        i += 1;
                    }
                }
            }
            i = item.append_view_slice(
                view_items,
                naming,
                min,
                max,
                i + 1,
                level + 1,
            );
            if i > max {
                return i;
            }
        }

        // If it has not been added yet, add it now.
        if i >= min {
            if let Some(node) = naming_extra {
                view_items.push(node);
                i += 1;
            }
        }

        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- FileNodeItem::cmp tests ---

    #[test]
    fn cmp_dir_before_file() {
        let dir = FileNodeItem::new_dir("/root/aaa", false);
        let file = FileNodeItem::new_file("/root/bbb");
        assert_eq!(dir.cmp(&file), Ordering::Less);
        assert_eq!(file.cmp(&dir), Ordering::Greater);
    }

    #[test]
    fn cmp_two_files_alphabetical() {
        let a = FileNodeItem::new_file("/root/apple.txt");
        let b = FileNodeItem::new_file("/root/banana.txt");
        assert_eq!(a.cmp(&b), Ordering::Less);
    }

    #[test]
    fn cmp_two_dirs_alphabetical() {
        let a = FileNodeItem::new_dir("/root/alpha", false);
        let b = FileNodeItem::new_dir("/root/beta", false);
        assert_eq!(a.cmp(&b), Ordering::Less);
    }

    #[test]
    fn cmp_human_sort_numbers() {
        let f2 = FileNodeItem::new_file("/root/file2.txt");
        let f10 = FileNodeItem::new_file("/root/file10.txt");
        assert_eq!(f2.cmp(&f10), Ordering::Less);
    }

    #[test]
    fn cmp_case_insensitive() {
        let upper = FileNodeItem::new_file("/root/Abc.txt");
        let lower = FileNodeItem::new_file("/root/abc.txt");
        assert_eq!(upper.cmp(&lower), Ordering::Equal);
    }

    // --- sorted_children ---

    #[test]
    fn sorted_children_dirs_before_files() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let file_a = FileNodeItem::new_file("/root/a_file.txt");
        let dir_b = FileNodeItem::new_dir("/root/b_dir", false);
        root.children.insert(file_a.path.clone(), file_a);
        root.children.insert(dir_b.path.clone(), dir_b);

        let sorted = root.sorted_children();
        assert!(sorted[0].is_dir);
        assert!(!sorted[1].is_dir);
    }

    #[test]
    fn sorted_children_empty() {
        let root = FileNodeItem::new_dir("/root", true);
        assert!(root.sorted_children().is_empty());
    }

    // --- ancestors_rev ---

    #[test]
    fn ancestors_rev_returns_correct_sequence() {
        let root = FileNodeItem::new_dir("/root", true);
        let ancestors: Vec<&Path> = root
            .ancestors_rev(Path::new("/root/a/b/c"))
            .unwrap()
            .collect();
        assert_eq!(
            ancestors,
            vec![
                Path::new("/root/a"),
                Path::new("/root/a/b"),
                Path::new("/root/a/b/c"),
            ]
        );
    }

    #[test]
    fn ancestors_rev_returns_none_for_unrelated_path() {
        let root = FileNodeItem::new_dir("/root", true);
        assert!(root.ancestors_rev(Path::new("/other/path")).is_none());
    }

    #[test]
    fn ancestors_rev_same_as_root() {
        let root = FileNodeItem::new_dir("/root", true);
        let ancestors: Vec<&Path> =
            root.ancestors_rev(Path::new("/root")).unwrap().collect();
        assert!(ancestors.is_empty());
    }

    // --- get_file_node ---

    #[test]
    fn get_file_node_self() {
        let root = FileNodeItem::new_dir("/root", true);
        let node = root.get_file_node(Path::new("/root")).unwrap();
        assert_eq!(node.path, PathBuf::from("/root"));
    }

    #[test]
    fn get_file_node_child() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let child = FileNodeItem::new_file("/root/file.txt");
        root.children.insert(child.path.clone(), child);

        let node = root.get_file_node(Path::new("/root/file.txt")).unwrap();
        assert_eq!(node.path, PathBuf::from("/root/file.txt"));
    }

    #[test]
    fn get_file_node_nested() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let mut sub = FileNodeItem::new_dir("/root/sub", true);
        let leaf = FileNodeItem::new_file("/root/sub/leaf.txt");
        sub.children.insert(leaf.path.clone(), leaf);
        root.children.insert(sub.path.clone(), sub);

        let node = root.get_file_node(Path::new("/root/sub/leaf.txt")).unwrap();
        assert_eq!(node.path, PathBuf::from("/root/sub/leaf.txt"));
    }

    #[test]
    fn get_file_node_missing_returns_none() {
        let root = FileNodeItem::new_dir("/root", true);
        assert!(root.get_file_node(Path::new("/root/missing")).is_none());
    }

    // --- add_child ---

    #[test]
    fn add_child_creates_node() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/new.txt"), false);

        let child = root.get_file_node(Path::new("/root/new.txt")).unwrap();
        assert_eq!(child.path, PathBuf::from("/root/new.txt"));
        assert!(!child.is_dir);
        assert!(!child.read);
        assert!(!child.open);
    }

    #[test]
    fn add_child_updates_count() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/a.txt"), false);
        root.add_child(Path::new("/root/b.txt"), false);
        assert_eq!(root.children_open_count, 2);
    }

    // --- remove_child ---

    #[test]
    fn remove_child_removes_node() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/file.txt"), false);
        assert!(root.get_file_node(Path::new("/root/file.txt")).is_some());

        let removed = root.remove_child(Path::new("/root/file.txt"));
        assert!(removed.is_some());
        assert!(root.get_file_node(Path::new("/root/file.txt")).is_none());
    }

    #[test]
    fn remove_child_updates_count() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/a.txt"), false);
        root.add_child(Path::new("/root/b.txt"), false);
        assert_eq!(root.children_open_count, 2);

        root.remove_child(Path::new("/root/a.txt"));
        assert_eq!(root.children_open_count, 1);
    }

    #[test]
    fn remove_child_missing_returns_none() {
        let mut root = FileNodeItem::new_dir("/root", true);
        assert!(root.remove_child(Path::new("/root/nope")).is_none());
    }

    // --- update_node_count ---

    #[test]
    fn update_node_count_closed_dir_is_zero() {
        let mut root = FileNodeItem::new_dir("/root", false);
        let child = FileNodeItem::new_file("/root/a.txt");
        root.children.insert(child.path.clone(), child);
        root.update_node_count(Path::new("/root"));
        assert_eq!(root.children_open_count, 0);
    }

    #[test]
    fn update_node_count_open_dir_counts_children() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let a = FileNodeItem::new_file("/root/a.txt");
        let b = FileNodeItem::new_file("/root/b.txt");
        root.children.insert(a.path.clone(), a);
        root.children.insert(b.path.clone(), b);
        root.update_node_count(Path::new("/root"));
        assert_eq!(root.children_open_count, 2);
    }

    #[test]
    fn update_node_count_recursive_nested() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let mut sub = FileNodeItem::new_dir("/root/sub", true);
        let leaf = FileNodeItem::new_file("/root/sub/leaf.txt");
        sub.children.insert(leaf.path.clone(), leaf);
        root.children.insert(sub.path.clone(), sub);

        // Update from leaf upward
        root.update_node_count(Path::new("/root/sub"));
        root.update_node_count(Path::new("/root"));
        // sub has 1 child (leaf), root has sub(1) + sub's children(1) = 2
        assert_eq!(
            root.get_file_node(Path::new("/root/sub"))
                .unwrap()
                .children_open_count,
            1
        );
        assert_eq!(root.children_open_count, 2);
    }

    // --- find_file_at_line ---

    #[test]
    fn find_file_at_line_in_flat_list() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/a.txt"), false);
        root.add_child(Path::new("/root/b.txt"), false);
        root.add_child(Path::new("/root/c.txt"), false);

        let (found, _line) = root.find_file_at_line(Path::new("/root/a.txt"));
        assert!(found);

        let (found, _line) = root.find_file_at_line(Path::new("/root/c.txt"));
        assert!(found);
    }

    #[test]
    fn find_file_at_line_not_found() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.add_child(Path::new("/root/a.txt"), false);

        let (found, _) = root.find_file_at_line(Path::new("/root/missing.txt"));
        assert!(!found);
    }

    #[test]
    fn find_file_at_line_closed_dir_not_found() {
        let root = FileNodeItem::new_dir("/root", false);
        let (found, line) = root.find_file_at_line(Path::new("/root/a.txt"));
        assert!(!found);
        assert_eq!(line, 0.0);
    }

    // --- NamingState tests ---

    #[test]
    fn naming_state_naming_accepts_input() {
        assert!(NamingState::Naming.is_accepting_input());
        assert!(!NamingState::Naming.is_err());
        assert!(NamingState::Naming.err().is_none());
    }

    #[test]
    fn naming_state_pending_does_not_accept_input() {
        assert!(!NamingState::Pending.is_accepting_input());
        assert!(!NamingState::Pending.is_err());
    }

    #[test]
    fn naming_state_err_accepts_input() {
        let state = NamingState::Err {
            err: "bad".to_string(),
        };
        assert!(state.is_accepting_input());
        assert!(state.is_err());
        assert_eq!(state.err(), Some("bad"));
    }

    #[test]
    fn naming_state_set_transitions() {
        let mut state = NamingState::Naming;

        state.set_err("oops".to_string());
        assert!(state.is_err());

        state.set_ok();
        assert!(!state.is_err());
        assert!(state.is_accepting_input());

        state.set_pending();
        assert!(!state.is_accepting_input());
    }

    // --- FileNodeViewKind::path() tests ---

    #[test]
    fn view_kind_path_returns_path() {
        let kind = FileNodeViewKind::Path(PathBuf::from("/a/b"));
        assert_eq!(kind.path(), Some(Path::new("/a/b")));
    }

    #[test]
    fn view_kind_renaming_returns_path() {
        let kind = FileNodeViewKind::Renaming {
            path: PathBuf::from("/a/b"),
            err: None,
        };
        assert_eq!(kind.path(), Some(Path::new("/a/b")));
    }

    #[test]
    fn view_kind_naming_returns_none() {
        let kind = FileNodeViewKind::Naming { err: None };
        assert_eq!(kind.path(), None);
    }

    #[test]
    fn view_kind_duplicating_returns_source() {
        let kind = FileNodeViewKind::Duplicating {
            source: PathBuf::from("/src/file"),
            err: Some("dup".into()),
        };
        assert_eq!(kind.path(), Some(Path::new("/src/file")));
    }

    // --- Naming delegating methods ---

    #[test]
    fn naming_none_state_is_none() {
        let naming = Naming::None;
        assert!(naming.state().is_none());
        assert!(!naming.is_accepting_input());
        assert!(!naming.editor_needs_reset());
        assert!(naming.as_renaming().is_none());
    }

    #[test]
    fn naming_renaming_state_delegates() {
        let mut naming = Naming::Renaming(Renaming {
            state: NamingState::Naming,
            path: PathBuf::from("/a"),
            editor_needs_reset: true,
        });
        assert!(naming.state().is_some());
        assert!(naming.is_accepting_input());
        assert!(naming.editor_needs_reset());
        assert!(naming.as_renaming().is_some());

        naming.set_editor_needs_reset(false);
        assert!(!naming.editor_needs_reset());

        naming.set_err("bad".into());
        assert!(naming.state().unwrap().is_err());

        naming.set_ok();
        assert!(!naming.state().unwrap().is_err());

        naming.set_pending();
        assert!(!naming.is_accepting_input());
    }

    #[test]
    fn naming_new_node_state_delegates() {
        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: true,
            base_path: PathBuf::from("/a"),
            editor_needs_reset: false,
        });
        assert!(naming.state().is_some());
        assert!(naming.is_accepting_input());
        assert!(!naming.editor_needs_reset());
        assert!(naming.as_renaming().is_none());
    }

    #[test]
    fn naming_duplicating_state_delegates() {
        let mut naming = Naming::Duplicating(Duplicating {
            state: NamingState::Naming,
            path: PathBuf::from("/a/b"),
            editor_needs_reset: true,
        });
        assert!(naming.state().is_some());
        assert!(naming.editor_needs_reset());
        naming.set_editor_needs_reset(false);
        assert!(!naming.editor_needs_reset());
    }

    // --- Naming::extra_node ---

    #[test]
    fn extra_node_new_node_matching_path() {
        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: false,
            base_path: PathBuf::from("/root"),
            editor_needs_reset: false,
        });
        let result = naming.extra_node(true, 1, Path::new("/root"));
        assert!(result.is_some());
        let data = result.unwrap();
        assert!(matches!(data.kind, FileNodeViewKind::Naming { err: None }));
        assert!(!data.is_dir); // Uses NewNode's is_dir, not the parameter
        assert!(!data.is_root);
        assert!(!data.open);
        assert_eq!(data.level, 2);
    }

    #[test]
    fn extra_node_new_node_with_error() {
        let naming = Naming::NewNode(NewNode {
            state: NamingState::Err {
                err: "exists".into(),
            },
            is_dir: true,
            base_path: PathBuf::from("/root"),
            editor_needs_reset: false,
        });
        let result = naming.extra_node(true, 1, Path::new("/root"));
        let data = result.unwrap();
        assert!(matches!(
            data.kind,
            FileNodeViewKind::Naming {
                err: Some(ref e)
            } if e == "exists"
        ));
        assert!(data.is_dir);
    }

    #[test]
    fn extra_node_duplicating_matching_path() {
        let naming = Naming::Duplicating(Duplicating {
            state: NamingState::Naming,
            path: PathBuf::from("/root/file.txt"),
            editor_needs_reset: false,
        });
        let result = naming.extra_node(false, 2, Path::new("/root/file.txt"));
        assert!(result.is_some());
        let data = result.unwrap();
        assert!(matches!(
            data.kind,
            FileNodeViewKind::Duplicating {
                ref source,
                err: None
            } if source == Path::new("/root/file.txt")
        ));
        assert!(!data.is_dir);
        assert_eq!(data.level, 3);
    }

    #[test]
    fn extra_node_non_matching_returns_none() {
        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: false,
            base_path: PathBuf::from("/other"),
            editor_needs_reset: false,
        });
        assert!(naming.extra_node(false, 1, Path::new("/root")).is_none());
    }

    #[test]
    fn extra_node_renaming_returns_none() {
        let naming = Naming::Renaming(Renaming {
            state: NamingState::Naming,
            path: PathBuf::from("/root/file.txt"),
            editor_needs_reset: false,
        });
        // Renaming never produces an extra_node
        assert!(
            naming
                .extra_node(false, 1, Path::new("/root/file.txt"))
                .is_none()
        );
    }

    // --- set_item_children ---

    #[test]
    fn set_item_children_opens_and_sets_children() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let sub = FileNodeItem::new_dir("/root/sub", false);
        root.children.insert(PathBuf::from("/root/sub"), sub);
        root.update_node_count(Path::new("/root"));

        let mut new_children = HashMap::new();
        new_children.insert(
            PathBuf::from("/root/sub/a.txt"),
            FileNodeItem::new_file("/root/sub/a.txt"),
        );
        new_children.insert(
            PathBuf::from("/root/sub/b.txt"),
            FileNodeItem::new_file("/root/sub/b.txt"),
        );

        root.set_item_children(Path::new("/root/sub"), new_children);

        let sub_node = root.get_file_node(Path::new("/root/sub")).unwrap();
        assert!(sub_node.open);
        assert!(sub_node.read);
        assert_eq!(sub_node.children.len(), 2);
        // Count should be propagated
        assert_eq!(sub_node.children_open_count, 2);
        assert_eq!(root.children_open_count, 3); // sub + 2 children
    }

    // --- find_file_at_line with nested open dirs ---

    #[test]
    fn find_file_at_line_nested_open_dir() {
        let mut root = FileNodeItem::new_dir("/root", true);

        let mut sub = FileNodeItem::new_dir("/root/sub", true);
        sub.children.insert(
            PathBuf::from("/root/sub/file.txt"),
            FileNodeItem::new_file("/root/sub/file.txt"),
        );
        sub.children_open_count = 1;

        root.children.insert(PathBuf::from("/root/sub"), sub);
        root.children_open_count = 2; // sub + file.txt

        // The file is at line 2 (sub=1, file.txt=2)
        let (found, line) = root.find_file_at_line(Path::new("/root/sub/file.txt"));
        assert!(found);
        assert_eq!(line, 2.0);
    }

    #[test]
    fn find_file_at_line_file_not_in_nested_dir() {
        let mut root = FileNodeItem::new_dir("/root", true);

        let mut sub = FileNodeItem::new_dir("/root/sub", true);
        sub.children.insert(
            PathBuf::from("/root/sub/other.txt"),
            FileNodeItem::new_file("/root/sub/other.txt"),
        );
        sub.children_open_count = 1;

        root.children.insert(PathBuf::from("/root/sub"), sub);
        root.children.insert(
            PathBuf::from("/root/top.txt"),
            FileNodeItem::new_file("/root/top.txt"),
        );
        root.children_open_count = 3;

        // top.txt is at line 3 (sub=1, other.txt=2, top.txt=3)
        let (found, line) = root.find_file_at_line(Path::new("/root/top.txt"));
        assert!(found);
        assert_eq!(line, 3.0);
    }

    // --- append_view_slice tests ---

    #[test]
    fn append_view_slice_skip_before_window() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.children.insert(
            PathBuf::from("/root/a.txt"),
            FileNodeItem::new_file("/root/a.txt"),
        );
        root.update_node_count(Path::new("/root"));

        let mut items = Vec::new();
        // Window starts at 100, so the root (at index 0) + children should be skipped
        let result =
            root.append_view_slice(&mut items, &Naming::None, 100, 200, 0, 1);
        assert!(items.is_empty());
        // Should return current + children_open_count
        assert_eq!(result, root.children_open_count);
    }

    #[test]
    fn append_view_slice_past_max_returns_early() {
        let root = FileNodeItem::new_file("/root/a.txt");
        let mut items = Vec::new();
        // current > max, should return immediately
        let result = root.append_view_slice(&mut items, &Naming::None, 0, 5, 10, 1);
        assert!(items.is_empty());
        assert_eq!(result, 10);
    }

    #[test]
    fn append_view_slice_renders_within_window() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.children.insert(
            PathBuf::from("/root/a.txt"),
            FileNodeItem::new_file("/root/a.txt"),
        );
        root.children.insert(
            PathBuf::from("/root/b.txt"),
            FileNodeItem::new_file("/root/b.txt"),
        );
        root.update_node_count(Path::new("/root"));

        let mut items = Vec::new();
        root.append_view_slice(&mut items, &Naming::None, 0, 100, 0, 1);

        // Should have root + 2 children = 3 items
        assert_eq!(items.len(), 3);
        // Root is level 1, is_root
        assert!(items[0].is_root);
        assert_eq!(items[0].level, 1);
        // Children are level 2, not is_root
        assert!(!items[1].is_root);
        assert_eq!(items[1].level, 2);
    }

    #[test]
    fn append_view_slice_renaming_injects_kind() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.children.insert(
            PathBuf::from("/root/a.txt"),
            FileNodeItem::new_file("/root/a.txt"),
        );
        root.update_node_count(Path::new("/root"));

        let naming = Naming::Renaming(Renaming {
            state: NamingState::Naming,
            path: PathBuf::from("/root/a.txt"),
            editor_needs_reset: false,
        });

        let mut items = Vec::new();
        root.append_view_slice(&mut items, &naming, 0, 100, 0, 1);

        // The file being renamed should have Renaming kind
        let file_item = items.iter().find(|i| {
            matches!(&i.kind, FileNodeViewKind::Renaming { path, .. } if path == Path::new("/root/a.txt"))
        });
        assert!(file_item.is_some());
    }

    // --- append_children_view_slice tests ---

    #[test]
    fn children_view_slice_closed_folder_no_naming() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let closed_sub = FileNodeItem::new_dir("/root/sub", false);
        root.children.insert(PathBuf::from("/root/sub"), closed_sub);
        root.update_node_count(Path::new("/root"));

        let mut items = Vec::new();
        let result =
            root.append_children_view_slice(&mut items, &Naming::None, 0, 100, 0, 1);
        // Closed sub is rendered by append_view_slice (called from append_children_view_slice)
        // but its children are not expanded
        assert_eq!(items.len(), 1); // just the closed sub
        assert!(!items[0].open);
        assert_eq!(result, 1);
    }

    #[test]
    fn children_view_slice_dir_naming_inserted_first() {
        let mut root = FileNodeItem::new_dir("/root", true);
        root.children.insert(
            PathBuf::from("/root/a.txt"),
            FileNodeItem::new_file("/root/a.txt"),
        );
        root.update_node_count(Path::new("/root"));

        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: true,
            base_path: PathBuf::from("/root"),
            editor_needs_reset: false,
        });

        let mut items = Vec::new();
        root.append_children_view_slice(&mut items, &naming, 0, 100, 0, 1);

        // The naming dir node should come first
        assert!(items.len() >= 2);
        assert!(matches!(items[0].kind, FileNodeViewKind::Naming { .. }));
        assert!(items[0].is_dir);
    }

    #[test]
    fn children_view_slice_file_naming_after_dirs() {
        let mut root = FileNodeItem::new_dir("/root", true);
        let sub = FileNodeItem::new_dir("/root/sub_dir", false);
        root.children.insert(PathBuf::from("/root/sub_dir"), sub);
        root.children.insert(
            PathBuf::from("/root/z.txt"),
            FileNodeItem::new_file("/root/z.txt"),
        );
        root.update_node_count(Path::new("/root"));

        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: false,
            base_path: PathBuf::from("/root"),
            editor_needs_reset: false,
        });

        let mut items = Vec::new();
        root.append_children_view_slice(&mut items, &naming, 0, 100, 0, 1);

        // Order should be: sub_dir, naming (file), z.txt
        assert!(items.len() >= 3);
        // First should be the directory
        assert!(matches!(
            &items[0].kind,
            FileNodeViewKind::Path(p) if p == Path::new("/root/sub_dir")
        ));
        // Second should be the naming node (file)
        assert!(matches!(items[1].kind, FileNodeViewKind::Naming { .. }));
        assert!(!items[1].is_dir);
    }

    #[test]
    fn children_view_slice_trailing_naming_node() {
        // If all children are directories and naming is a file, it appears at the end
        let mut root = FileNodeItem::new_dir("/root", true);
        root.children.insert(
            PathBuf::from("/root/dir_a"),
            FileNodeItem::new_dir("/root/dir_a", false),
        );
        root.update_node_count(Path::new("/root"));

        let naming = Naming::NewNode(NewNode {
            state: NamingState::Naming,
            is_dir: false,
            base_path: PathBuf::from("/root"),
            editor_needs_reset: false,
        });

        let mut items = Vec::new();
        root.append_children_view_slice(&mut items, &naming, 0, 100, 0, 1);

        // dir_a first, then naming node at the end
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0].kind,
            FileNodeViewKind::Path(p) if p == Path::new("/root/dir_a")
        ));
        assert!(matches!(items[1].kind, FileNodeViewKind::Naming { .. }));
    }

    // --- PathObject constructors ---

    #[test]
    fn path_object_new_has_linecol() {
        let po = PathObject::new(PathBuf::from("/a/b"), false, 10, 5);
        assert_eq!(po.path, PathBuf::from("/a/b"));
        assert!(!po.is_dir);
        assert_eq!(
            po.linecol,
            Some(LineCol {
                line: 10,
                column: 5
            })
        );
    }

    #[test]
    fn path_object_from_path_no_linecol() {
        let po = PathObject::from_path(PathBuf::from("/c"), true);
        assert_eq!(po.path, PathBuf::from("/c"));
        assert!(po.is_dir);
        assert!(po.linecol.is_none());
    }
}
