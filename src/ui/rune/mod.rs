//! The Rune editor. `face` draws the screen and `host` owns the macOS window.

mod face;
mod host;

pub use host::PluginEditor;
pub(crate) use host::{
    EditorState, Repaint, is_open, new_repaint, new_state, open, request_repaint,
};
