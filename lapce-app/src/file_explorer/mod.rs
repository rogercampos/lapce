// File explorer: a tree view of the workspace's file system. Uses lazy directory
// loading (directories are read from the proxy only when first expanded) and a
// pre-computed children_open_count for efficient virtual scrolling. The tree is
// stored as a single recursive FileNodeItem signal; file operations (rename,
// create, delete, duplicate) are mediated through InternalCommands.
pub mod data;
pub mod node;
pub mod view;
