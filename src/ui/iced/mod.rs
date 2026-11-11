//! The iced editor.
//!
//! wgpu only: the software renderer redraws the whole window on macOS and
//! spikes the CPU while the playhead moves. The window is not redrawn unless
//! the audio thread (or a gesture) sets the flag. The poll is only how long
//! that flag can wait before the editor paints.

mod face;

use std::sync::Arc;
use std::time::Duration;

use nice_plug::editor::dpi::LogicalSize;
use nice_plug::prelude::ResizeHint;
use nice_plug_iced::iced::{PollSubNotifier, Theme};
use nice_plug_iced::{
    IcedEditor, IcedEditorState, IcedNiceSettings, application, create_iced_editor,
};

use crate::editor::SonomoriEditor;

use face::{Screen, window_style};

/// Longest a playhead step waits before the editor paints it.
const POLL: Duration = Duration::from_millis(30);

pub type EditorState = IcedEditorState;
pub type Repaint = PollSubNotifier;
pub type PluginEditor = IcedEditor;

pub fn new_state(size: LogicalSize<f32>) -> Arc<EditorState> {
    EditorState::from_size(size, 1.0)
}

pub fn new_repaint() -> Repaint {
    Repaint::new()
}

#[inline]
pub fn is_open(state: &EditorState) -> bool {
    state.is_open()
}

#[inline]
pub fn request_repaint(repaint: &Repaint) {
    repaint.notify();
}

pub fn open(
    state: Arc<EditorState>,
    repaint: Repaint,
    title: &str,
    resize: ResizeHint,
    editor: SonomoriEditor,
) -> Option<PluginEditor> {
    let repaint_for_boot = repaint.clone();
    create_iced_editor(
        state,
        editor,
        repaint,
        IcedNiceSettings::new()
            .with_tile(title)
            .with_resize_hint(resize)
            .with_always_redraw(false)
            .with_poll_interval(POLL),
        move |persistent, ctx| {
            let repaint = repaint_for_boot.clone();
            let program = application(
                persistent,
                ctx,
                move |persistent, ctx| Screen::boot(persistent, ctx, repaint.clone()),
                Screen::update,
                Screen::view,
            )
            .antialiasing(false)
            .theme(Theme::Dark)
            .style(window_style)
            .subscription(Screen::subscription)
            .run();
            Ok(program)
        },
    )
}
