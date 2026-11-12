use std::cell::Cell;

use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::ParamPtr;
use nice_plug_iced::iced::widget::canvas;
use nice_plug_iced::iced::{self, Rectangle, Renderer, Theme};

use super::marks::fill_round;
use super::{ACCENT, Chrome, Message, TRACK, TRACK_HEIGHT};

const GRANULAR_DRAG: f32 = 0.0015;

#[derive(Clone, Copy)]
pub(super) struct DragMem {
    ptr: ParamPtr,
    origin_norm: f32,
    origin_x: f32,
    shift: bool,
    start_norm: f32,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::ui::iced) enum SliderEvent {
    Down { ptr: ParamPtr, x: f32 },
    Move { ptr: ParamPtr, x: f32, width: f32 },
    Up { ptr: ParamPtr, x: f32, width: f32 },
    Reset { ptr: ParamPtr },
    Cancel { ptr: ParamPtr },
}

fn drag_norm(origin_norm: f32, origin_x: f32, x: f32, width: f32, shift: bool) -> f32 {
    if shift {
        (origin_norm + (x - origin_x) * GRANULAR_DRAG).clamp(0.0, 1.0)
    } else if width > 0.0 {
        (x / width).clamp(0.0, 1.0)
    } else {
        origin_norm
    }
}

pub(super) struct Track {
    pub(super) ptr: ParamPtr,
    pub(super) fraction: f32,
}

#[derive(Default)]
pub(super) struct TrackState {
    dragging: bool,
    previous: Option<iced::core::mouse::Click>,
    cache: canvas::Cache,
    fraction_bits: Cell<u32>,
}

impl canvas::Program<Message, Theme, Renderer> for Track {
    type State = TrackState;

    fn update(
        &self,
        state: &mut TrackState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let cursor = cursor.land();
        match event {
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                let Some(position) = cursor.position() else {
                    return None;
                };
                if !state.dragging && !bounds.contains(position) {
                    return None;
                }
                let previous = state.previous;
                let click =
                    iced::core::mouse::Click::new(position, iced::mouse::Button::Left, previous);
                state.previous = Some(click);
                if click.kind() == iced::core::mouse::click::Kind::Double {
                    state.dragging = false;
                    return Some(
                        canvas::Action::publish(Message::Slider(SliderEvent::Reset {
                            ptr: self.ptr,
                        }))
                        .and_capture(),
                    );
                }
                state.dragging = true;
                Some(
                    canvas::Action::publish(Message::Slider(SliderEvent::Down {
                        ptr: self.ptr,
                        x: position.x - bounds.x,
                    }))
                    .and_capture(),
                )
            }
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { .. }) if state.dragging => {
                let Some(position) = cursor.position() else {
                    return None;
                };
                Some(
                    canvas::Action::publish(Message::Slider(SliderEvent::Move {
                        ptr: self.ptr,
                        x: position.x - bounds.x,
                        width: bounds.width,
                    }))
                    .and_capture(),
                )
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left))
                if state.dragging =>
            {
                state.dragging = false;
                let x = cursor
                    .position()
                    .map(|position| position.x - bounds.x)
                    .unwrap_or(0.0);
                Some(
                    canvas::Action::publish(Message::Slider(SliderEvent::Up {
                        ptr: self.ptr,
                        x,
                        width: bounds.width,
                    }))
                    .and_capture(),
                )
            }
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) if state.dragging => {
                state.dragging = false;
                Some(
                    canvas::Action::publish(Message::Slider(SliderEvent::Cancel { ptr: self.ptr }))
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &TrackState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let bits = self.fraction.clamp(0.0, 1.0).to_bits();
        if state.fraction_bits.get() != bits {
            state.cache.clear();
            state.fraction_bits.set(bits);
        }
        let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
            let radius = TRACK_HEIGHT * 0.5;
            fill_round(frame, 0.0, 0.0, bounds.width, bounds.height, radius, TRACK);
            let fill_w = (self.fraction.clamp(0.0, 1.0) * bounds.width).max(0.0);
            if fill_w >= 0.5 {
                fill_round(frame, 0.0, 0.0, fill_w, bounds.height, radius, ACCENT);
            }
        });
        vec![geometry]
    }

    fn mouse_interaction(
        &self,
        state: &TrackState,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        if state.dragging || cursor.is_over(bounds) {
            iced::mouse::Interaction::ResizingHorizontally
        } else {
            iced::mouse::Interaction::default()
        }
    }
}

pub(super) fn apply_slider(gui: &GuiContext, chrome: &Chrome, event: SliderEvent) {
    match event {
        SliderEvent::Down { ptr, x } => {
            if chrome.command.get() {
                begin(gui, ptr);
                reset(gui, ptr);
                end(gui, ptr);
                chrome.drag.set(None);
                return;
            }
            let start = normalized(ptr);
            chrome.drag.set(Some(DragMem {
                ptr,
                origin_norm: start,
                origin_x: x,
                shift: chrome.shift.get(),
                start_norm: start,
            }));
            begin(gui, ptr);
        }
        SliderEvent::Move { ptr, x, width } => drag_to(gui, chrome, ptr, x, width, false),
        SliderEvent::Up { ptr, x, width } => drag_to(gui, chrome, ptr, x, width, true),
        SliderEvent::Reset { ptr } => {
            let open = chrome.drag.get().is_some_and(|memory| memory.ptr == ptr);
            if !open {
                begin(gui, ptr);
            }
            reset(gui, ptr);
            if !open {
                end(gui, ptr);
            }
        }
        SliderEvent::Cancel { ptr } => {
            let Some(memory) = chrome.drag.get() else {
                return;
            };
            if memory.ptr != ptr {
                return;
            }
            restore(gui, ptr, memory.start_norm);
            end(gui, ptr);
            chrome.drag.set(None);
        }
    }
}

fn drag_to(gui: &GuiContext, chrome: &Chrome, ptr: ParamPtr, x: f32, width: f32, finish: bool) {
    let Some(mut memory) = chrome.drag.get() else {
        return;
    };
    if memory.ptr != ptr {
        return;
    }
    let shift = chrome.shift.get();
    if shift != memory.shift {
        if shift {
            memory.origin_norm = normalized(ptr);
            memory.origin_x = x;
        }
        memory.shift = shift;
    }
    let norm = drag_norm(memory.origin_norm, memory.origin_x, x, width, memory.shift);
    set_snapped(gui, ptr, norm);
    if finish {
        end(gui, ptr);
        chrome.drag.set(None);
    } else {
        chrome.drag.set(Some(memory));
    }
}

/// `ptr` addresses a field of the editor's `SonomoriParams`. The gesture runs
/// on the GUI thread while that object is alive.
fn begin(gui: &GuiContext, ptr: ParamPtr) {
    unsafe { gui.raw_begin_set_parameter(ptr) }
}

fn end(gui: &GuiContext, ptr: ParamPtr) {
    unsafe { gui.raw_end_set_parameter(ptr) }
}

fn normalized(ptr: ParamPtr) -> f32 {
    unsafe { ptr.modulated_normalized_value() }
}

fn set_snapped(gui: &GuiContext, ptr: ParamPtr, norm: f32) {
    unsafe {
        let plain = ptr.preview_plain(norm);
        if plain != ptr.modulated_plain_value() {
            let snapped = ptr.preview_normalized(plain);
            gui.raw_set_parameter_normalized(ptr, snapped);
        }
    }
}

fn reset(gui: &GuiContext, ptr: ParamPtr) {
    unsafe {
        let plain = ptr.default_plain_value();
        gui.raw_set_parameter_normalized(ptr, ptr.preview_normalized(plain));
    }
}

fn restore(gui: &GuiContext, ptr: ParamPtr, norm: f32) {
    unsafe { gui.raw_set_parameter_normalized(ptr, norm) }
}
