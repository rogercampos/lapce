use std::collections::HashSet;
use std::path::PathBuf;

use floem::views::VirtualVector;
use lapce_rpc::file::{FileNodeItem, FileNodeViewData, Naming};

/// Adapter that makes the recursive FileNodeItem tree usable with Floem's
/// virtual_stack. The total_len is O(1) thanks to the pre-computed
/// children_open_count. The slice method delegates to FileNodeItem::append_view_slice
/// which walks the tree to extract only the visible items in the requested range.
pub struct FileNodeVirtualList {
    file_node_item: FileNodeItem,
    naming: Naming,
    starred: HashSet<PathBuf>,
}

impl FileNodeVirtualList {
    pub fn new(
        file_node_item: FileNodeItem,
        naming: Naming,
        starred: HashSet<PathBuf>,
    ) -> Self {
        Self {
            file_node_item,
            naming,
            starred,
        }
    }
}

impl VirtualVector<FileNodeViewData> for FileNodeVirtualList {
    fn total_len(&self) -> usize {
        self.file_node_item.children_open_count + 1
    }

    fn slice(
        &mut self,
        range: std::ops::Range<usize>,
    ) -> impl Iterator<Item = FileNodeViewData> {
        let naming = &self.naming;
        let root = &self.file_node_item;

        let min = range.start;
        let max = range.end;
        let mut view_items = Vec::new();

        root.append_view_slice_starred(
            &mut view_items,
            naming,
            min,
            max,
            0,
            1,
            &self.starred,
        );

        view_items.into_iter()
    }
}
