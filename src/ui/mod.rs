//! The editor draws through the `Surface` trait in this module.
//!
//! Files under this folder, other than `face`, are the only source files that
//! may name a UI library. `ui` chooses which of those implementations the
//! build links. The shell functions are what the plugin calls to open the
//! window and to wake it from the audio thread.

mod face;
mod ui;

#[cfg(test)]
mod seam;

#[cfg(ui_iced)]
mod iced;
#[cfg(ui_iced)]
use iced as backend;

pub use backend::PluginEditor;
pub(crate) use backend::{
    EditorState, Repaint, is_open, new_repaint, new_state, open, request_repaint,
};
pub(crate) use face::{ACCENT, Breadth, Color, MUTED, Painter, Point, Rect, Sense, Surface, TRACK};
