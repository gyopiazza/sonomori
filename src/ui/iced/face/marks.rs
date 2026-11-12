use std::cell::{Cell, RefCell};
use std::sync::atomic::Ordering;

use nice_plug_iced::iced::widget::canvas::{self, Path, Stroke};
use nice_plug_iced::iced::{self, Pixels, Rectangle, Renderer, Theme};

use crate::ui::{Color, Painter, Point, Rect};

use super::{MARK_HASH, Message, ink, mix};

pub(super) fn scale_x(virtual_w: f32, bounds_w: f32, x: f32) -> f32 {
    if virtual_w > 0.0 {
        x * bounds_w / virtual_w
    } else {
        x
    }
}

pub(super) fn unscale_x(virtual_w: f32, bounds_w: f32, local_x: f32) -> f32 {
    if bounds_w > 0.0 {
        local_x * virtual_w / bounds_w
    } else {
        local_x
    }
}

#[derive(Clone)]
pub(super) enum Mark {
    Fill {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: Color,
    },
    Lines {
        width: f32,
        color: Color,
        segments: Vec<[f32; 4]>,
    },
    Text {
        x: f32,
        y: f32,
        size: f32,
        color: Color,
        text: String,
    },
}

fn color_bits(color: Color) -> u64 {
    u32::from_le_bytes([color.r, color.g, color.b, color.a]) as u64
}

pub(super) struct MarkPainter {
    pub(super) marks: Vec<Mark>,
    lines: Option<LineBatch>,
    pub(super) fingerprint: u64,
    /// Playhead. Kept out of `fingerprint` so the cells under it stay cached.
    pub(super) overlay: Option<Mark>,
    pub(super) overlay_fp: u64,
}

impl MarkPainter {
    pub(super) fn new(marks: Vec<Mark>) -> Self {
        Self {
            marks,
            lines: None,
            fingerprint: MARK_HASH,
            overlay: None,
            overlay_fp: MARK_HASH,
        }
    }
}

impl Default for MarkPainter {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

struct LineBatch {
    width: f32,
    color: Color,
    segments: Vec<[f32; 4]>,
}

impl MarkPainter {
    pub(super) fn finish(&mut self) {
        self.flush_lines();
        self.fingerprint = hash_marks(&self.marks);
        self.overlay_fp = self
            .overlay
            .as_ref()
            .map(|mark| hash_mark(MARK_HASH, mark))
            .unwrap_or(MARK_HASH);
    }

    fn flush_lines(&mut self) {
        let Some(batch) = self.lines.take() else {
            return;
        };
        if batch.segments.is_empty() {
            return;
        }
        self.marks.push(Mark::Lines {
            width: batch.width,
            color: batch.color,
            segments: batch.segments,
        });
    }
}

impl Painter for MarkPainter {
    fn fill(&mut self, rect: Rect, radius: f32, color: Color) {
        self.flush_lines();
        let (w, h) = (rect.width(), rect.height());
        if w < 0.5 || h < 0.5 {
            return;
        }
        self.marks.push(Mark::Fill {
            x: rect.left(),
            y: rect.top(),
            w,
            h,
            radius,
            color,
        });
    }

    fn line(&mut self, from: Point, to: Point, width: f32, color: Color) {
        let same = self
            .lines
            .as_ref()
            .is_some_and(|batch| batch.width == width && batch.color == color);
        if !same {
            self.flush_lines();
            self.lines = Some(LineBatch {
                width,
                color,
                // The waveform is 512 columns. One allocation covers it.
                segments: Vec::with_capacity(512),
            });
        }
        if let Some(batch) = self.lines.as_mut() {
            batch.segments.push([from.x, from.y, to.x, to.y]);
        }
    }

    fn text_centered(&mut self, at: Point, text_value: &str, size: f32, color: Color) {
        self.flush_lines();
        self.marks.push(Mark::Text {
            x: at.x,
            y: at.y,
            size,
            color,
            text: text_value.to_owned(),
        });
    }

    fn highlight(&mut self, rect: Rect, radius: f32, color: Color) {
        self.flush_lines();
        let (w, h) = (rect.width(), rect.height());
        if w < 0.5 || h < 0.5 {
            return;
        }
        self.overlay = Some(Mark::Fill {
            x: rect.left(),
            y: rect.top(),
            w,
            h,
            radius,
            color,
        });
    }
}

pub(super) struct Marks {
    marks: Vec<Mark>,
    overlay: Option<Mark>,
    virtual_w: f32,
    id: Option<u32>,
    fingerprint: u64,
    overlay_fp: u64,
}

#[derive(Default)]
pub(super) struct MarksState {
    armed: bool,
    cache: canvas::Cache,
    overlay_cache: canvas::Cache,
    fingerprint: Cell<u64>,
    overlay_fp: Cell<u64>,
    /// Last base marks. A resize retessellates from here when the live frame
    /// recycled its buffer after a fingerprint hit.
    stored: RefCell<Vec<Mark>>,
}

pub(super) fn marks_canvas(
    marks: Vec<Mark>,
    overlay: Option<Mark>,
    virtual_w: f32,
    id: Option<u32>,
    fingerprint: u64,
    overlay_fp: u64,
) -> canvas::Canvas<Marks, Message, Theme, Renderer> {
    iced::widget::canvas(Marks {
        marks,
        overlay,
        virtual_w,
        id,
        fingerprint,
        overlay_fp,
    })
}

impl canvas::Program<Message, Theme, Renderer> for Marks {
    type State = MarksState;

    fn update(
        &self,
        state: &mut MarksState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let Some(id) = self.id else {
            return None;
        };
        let cursor = cursor.land();
        match event {
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                let inside = cursor
                    .position()
                    .is_some_and(|point| bounds.contains(point));
                if !inside {
                    state.armed = false;
                    return None;
                }
                state.armed = true;
                Some(canvas::Action::capture())
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left))
                if state.armed =>
            {
                state.armed = false;
                let Some(position) = cursor.position() else {
                    return Some(canvas::Action::capture());
                };
                let x = unscale_x(self.virtual_w, bounds.width, position.x - bounds.x);
                let y = position.y - bounds.y;
                Some(canvas::Action::publish(Message::Canvas { id, x, y }).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &MarksState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        // The widget state outlives this program. Unchanged marks keep the
        // tessellated mesh. The playhead is a second mesh so a step does not
        // throw away the cells underneath it.
        if state.fingerprint.get() != self.fingerprint {
            state.cache.clear();
            state.fingerprint.set(self.fingerprint);
            state.stored.borrow_mut().clone_from(&self.marks);
            super::COUNTERS.misses.fetch_add(1, Ordering::Relaxed);
        } else {
            super::COUNTERS.hits.fetch_add(1, Ordering::Relaxed);
        }
        let base = state.cache.draw(renderer, bounds.size(), |frame| {
            if self.marks.is_empty() {
                paint_marks(frame, &state.stored.borrow(), self.virtual_w, bounds.width);
            } else {
                paint_marks(frame, &self.marks, self.virtual_w, bounds.width);
            }
        });
        let Some(overlay) = &self.overlay else {
            return vec![base];
        };
        if state.overlay_fp.get() != self.overlay_fp {
            state.overlay_cache.clear();
            state.overlay_fp.set(self.overlay_fp);
        }
        let highlight = state.overlay_cache.draw(renderer, bounds.size(), |frame| {
            paint_marks(
                frame,
                std::slice::from_ref(overlay),
                self.virtual_w,
                bounds.width,
            );
        });
        vec![base, highlight]
    }

    fn mouse_interaction(
        &self,
        _state: &MarksState,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        if self.id.is_some() && cursor.is_over(bounds) {
            iced::mouse::Interaction::Pointer
        } else {
            iced::mouse::Interaction::default()
        }
    }
}

fn hash_marks(marks: &[Mark]) -> u64 {
    marks
        .iter()
        .fold(MARK_HASH, |hash, mark| hash_mark(hash, mark))
}

fn hash_mark(mut hash: u64, mark: &Mark) -> u64 {
    match mark {
        Mark::Fill {
            x,
            y,
            w,
            h,
            radius,
            color,
        } => {
            hash = mix(hash, 1);
            hash = mix(hash, x.to_bits() as u64);
            hash = mix(hash, y.to_bits() as u64);
            hash = mix(hash, w.to_bits() as u64);
            hash = mix(hash, h.to_bits() as u64);
            hash = mix(hash, radius.to_bits() as u64);
            mix(hash, color_bits(*color))
        }
        Mark::Lines {
            width,
            color,
            segments,
        } => {
            hash = mix(hash, 2);
            hash = mix(hash, width.to_bits() as u64);
            hash = mix(hash, color_bits(*color));
            for segment in segments {
                for value in segment {
                    hash = mix(hash, value.to_bits() as u64);
                }
            }
            hash
        }
        Mark::Text {
            x,
            y,
            size,
            color,
            text,
        } => {
            hash = mix(hash, 3);
            hash = mix(hash, x.to_bits() as u64);
            hash = mix(hash, y.to_bits() as u64);
            hash = mix(hash, size.to_bits() as u64);
            hash = mix(hash, color_bits(*color));
            for byte in text.bytes() {
                hash = mix(hash, byte as u64);
            }
            hash
        }
    }
}

fn paint_marks(frame: &mut canvas::Frame, marks: &[Mark], virtual_w: f32, bounds_w: f32) {
    for mark in marks {
        match mark {
            Mark::Fill {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                let x = scale_x(virtual_w, bounds_w, *x);
                let w = scale_x(virtual_w, bounds_w, *w);
                fill_round(frame, x, *y, w, *h, *radius, *color);
            }
            Mark::Lines {
                width,
                color,
                segments,
            } => {
                let path = Path::new(|builder| {
                    for segment in segments {
                        builder.move_to(iced::Point::new(
                            scale_x(virtual_w, bounds_w, segment[0]),
                            segment[1],
                        ));
                        builder.line_to(iced::Point::new(
                            scale_x(virtual_w, bounds_w, segment[2]),
                            segment[3],
                        ));
                    }
                });
                frame.stroke(
                    &path,
                    Stroke::default().with_color(ink(*color)).with_width(*width),
                );
            }
            Mark::Text {
                x,
                y,
                size,
                color,
                text,
            } => {
                frame.fill_text(canvas::Text {
                    content: text.clone(),
                    position: iced::Point::new(scale_x(virtual_w, bounds_w, *x), *y),
                    max_width: bounds_w,
                    color: ink(*color),
                    size: Pixels(*size),
                    line_height: iced::widget::text::LineHeight::Relative(1.0),
                    font: iced::Font::default(),
                    align_x: iced::widget::text::Alignment::Center,
                    align_y: iced::alignment::Vertical::Center,
                    shaping: iced::widget::text::Shaping::Basic,
                });
            }
        }
    }
}

pub(super) fn fill_round(
    frame: &mut canvas::Frame,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
) {
    if w < 0.5 || h < 0.5 {
        return;
    }
    let radius = radius.min(w * 0.5).min(h * 0.5);
    let top_left = iced::Point::new(x, y);
    let size = iced::Size::new(w, h);
    let path = if radius < 0.5 {
        Path::rectangle(top_left, size)
    } else {
        Path::rounded_rectangle(top_left, size, iced::border::Radius::new(radius))
    };
    frame.fill(&path, ink(color));
}
