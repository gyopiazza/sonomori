//! The editor draws through the `Surface` trait in this module.
//!
//! Files under this folder, other than `face`, are the only source files that
//! may name a UI library. `ui` chooses which of those implementations
//! the build links. The shell functions are what the plugin calls to open the
//! window and to wake it from the audio thread.

mod face;
mod ui;

#[cfg(test)]
mod seam;

#[cfg(ui_egui)]
mod egui;
#[cfg(ui_egui)]
use egui as backend;

#[cfg(all(ui_rune, target_os = "macos"))]
mod rune;
#[cfg(all(ui_rune, target_os = "macos"))]
use rune as backend;

#[cfg(all(ui_rune, not(target_os = "macos")))]
compile_error!(
    "src/ui/ui.rs selects the macOS editor backend. Set BACKEND to Ui::Egui to build on this target."
);

pub use backend::PluginEditor;
pub(crate) use backend::{
    EditorState, Repaint, is_open, new_repaint, new_state, open, request_repaint,
};
pub(crate) use face::{Breadth, Color, Painter, Point, Rect, Sense, Surface};
