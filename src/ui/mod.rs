//! The editor draws through the `Surface` trait in this module.
//!
//! `backend` is the only source file that may name a UI library. Replace that
//! file and the UI-library dependencies in Cargo.toml to swap libraries. The
//! shell functions are what the plugin calls to open the window and to wake
//! it from the audio thread.

mod backend;
mod face;

pub use backend::PluginEditor;
pub(crate) use backend::{
    EditorState, Repaint, is_open, new_repaint, new_state, open, request_repaint,
};
pub(crate) use face::{Breadth, Color, Painter, Point, Rect, Sense, Surface};
