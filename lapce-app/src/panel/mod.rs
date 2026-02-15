// The panel module implements the side/bottom panel system. Panels are docked UI
// regions (left, right, bottom) that host tool views like File Explorer and Search.
// Each panel container holds two positions (e.g. LeftTop/LeftBottom) that can each
// display a tabbed set of panel kinds. The layout is fixed -- no drag-and-drop
// reordering -- and panel visibility/state is persisted per-workspace.
pub mod data;
pub mod global_search_view;
pub mod kind;
pub mod position;
pub mod style;
pub mod view;
