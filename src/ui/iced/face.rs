//! The editor screen, drawn with iced.
//!
//! `Screen` latches input while iced updates, then the next view reads those
//! latches through [`Surface`]. Waveform and pattern marks are recorded in a
//! virtual width and replayed into one canvas frame, with X scaled to the
//! laid-out width and stroke widths left in pixels. Slider tracks are a
//! separate canvas so a drag that leaves the 14 px track still moves.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::{FloatParam, IntParam, Param, ParamSetter};
use nice_plug_iced::iced::widget::canvas::{self, Path, Stroke};
use nice_plug_iced::iced::widget::{button, column, container, row, stack, text, text_input};
use nice_plug_iced::iced::{
    self, Element, Length, Pixels, PollSubNotifier, Rectangle, Renderer, Subscription, Task, Theme,
};
use nice_plug_iced::{IcedNiceContext, PersistentState};

use crate::editor::SonomoriEditor;
use crate::params::SonomoriParams;
use crate::ui::{Breadth, Color, Painter, Point, Rect, Sense, Surface};

const COLUMN_GAP: f32 = 6.0;
const ROW_GAP: f32 = 8.0;
const PADDING: f32 = 8.0;
const READOUT_WIDTH: f32 = 84.0;
const TRACK_HEIGHT: f32 = 14.0;
const METER_HEIGHT: f32 = 16.0;
/// Fill-width canvases are painted in this width. A click is mapped back by
/// the same ratio the frame uses to place marks.
const VIRTUAL_WIDTH: f32 = 1000.0;
const GRANULAR_DRAG: f32 = 0.0015;
const BODY: f32 = 13.0;
const HEADING: f32 = 20.0;

const TEXT: Color = Color::rgb(232, 232, 236);
const MUTED: Color = Color::rgb(140, 144, 150);
const PAPER: Color = Color::rgb(24, 24, 27);
const BUTTON: Color = Color::rgb(46, 48, 54);
const BUTTON_OFF: Color = Color::rgb(36, 38, 42);
const TRACK: Color = Color::rgb(38, 40, 46);
const TEAL: Color = Color::rgb(86, 196, 184);

type El<'a> = Element<'a, Message, Theme, Renderer>;

pub(super) struct Screen {
    editor: RefCell<PersistentState<SonomoriEditor>>,
    ctx: IcedNiceContext,
    repaint: PollSubNotifier,
    chrome: Chrome,
}

struct Chrome {
    path: RefCell<String>,
    path_seen: RefCell<String>,
    pressed: Cell<Option<u32>>,
    canvas: Cell<Option<(u32, f32, f32)>>,
    drag: Cell<Option<DragMem>>,
    shift: Cell<bool>,
    command: Cell<bool>,
    wants_frame: Cell<bool>,
    ids: Cell<u32>,
    sliders: Cell<u32>,
    #[cfg(test)]
    log: RefCell<LayoutLog>,
}

#[derive(Clone, Copy)]
struct DragMem {
    index: u8,
    origin_norm: f32,
    origin_x: f32,
    shift: bool,
    start_norm: f32,
}

#[derive(Clone, Debug)]
pub(super) enum Message {
    Press(u32),
    Path(String),
    Poll,
    Resized,
    Mods(iced::keyboard::Modifiers),
    Slider(SliderEvent),
    Canvas { id: u32, x: f32, y: f32 },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SliderEvent {
    Down {
        index: u8,
        x: f32,
        /// Present so a down event has the same geometry as a move. The
        /// gesture records `x` and reads the width on the later moves.
        #[allow(dead_code)]
        width: f32,
    },
    Move {
        index: u8,
        x: f32,
        width: f32,
    },
    Up {
        index: u8,
        x: f32,
        width: f32,
    },
    Reset {
        index: u8,
    },
    Cancel {
        index: u8,
    },
}

#[cfg(test)]
#[derive(Default)]
struct LayoutLog {
    readouts: Vec<Length>,
    tracks: Vec<Length>,
    hint: bool,
}

impl Screen {
    pub(super) fn boot(
        mut persistent: PersistentState<SonomoriEditor>,
        ctx: IcedNiceContext,
        repaint: PollSubNotifier,
    ) -> Self {
        persistent.attach_gui(ctx.nice_context.clone());
        ctx.sync_window_size();
        Self {
            editor: RefCell::new(persistent),
            ctx,
            repaint,
            chrome: Chrome::new(),
        }
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Press(id) => self.chrome.pressed.set(Some(id)),
            Message::Path(value) => *self.chrome.path.borrow_mut() = value,
            Message::Poll => {}
            Message::Resized => self.ctx.sync_window_size(),
            Message::Mods(mods) => {
                self.chrome.shift.set(mods.shift());
                self.chrome.command.set(mods.command());
            }
            Message::Slider(event) => {
                let (gui, params) = {
                    let editor = self.editor.borrow();
                    (editor.gui(), Arc::clone(editor.params()))
                };
                if let Some(gui) = gui {
                    apply_slider(&gui, &params, &self.chrome, event);
                }
            }
            Message::Canvas { id, x, y } => self.chrome.canvas.set(Some((id, x, y))),
        }
        Task::none()
    }

    pub(super) fn view(&self) -> El<'_> {
        self.chrome.begin_frame();
        let children = {
            let mut editor = self.editor.borrow_mut();
            let gui = editor.gui();
            let mut face = Face {
                chrome: &self.chrome,
                children: Vec::new(),
            };
            if let Some(gui) = gui {
                let setter = gui.param_setter();
                editor.show(&mut face, &setter);
            }
            face.children
        };
        let root = container(
            column(children)
                .spacing(COLUMN_GAP)
                .padding(PADDING)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(paper);
        self.chrome.finish_frame(&self.repaint);
        root.into()
    }

    pub(super) fn subscription(_state: &Self) -> Subscription<Message> {
        Subscription::batch([
            iced::poll_events().map(|()| Message::Poll),
            iced::window_resized().map(|()| Message::Resized),
            iced::event::listen_with(on_mods),
        ])
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.editor.borrow_mut().detach_gui();
    }
}

pub(super) fn window_style(_state: &Screen, _theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: ink(PAPER),
        text_color: ink(TEXT),
    }
}

fn on_mods(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(mods)) => {
            Some(Message::Mods(mods))
        }
        _ => None,
    }
}

impl Chrome {
    fn new() -> Self {
        Self {
            path: RefCell::new(String::new()),
            path_seen: RefCell::new(String::new()),
            pressed: Cell::new(None),
            canvas: Cell::new(None),
            drag: Cell::new(None),
            shift: Cell::new(false),
            command: Cell::new(false),
            wants_frame: Cell::new(false),
            ids: Cell::new(0),
            sliders: Cell::new(0),
            #[cfg(test)]
            log: RefCell::new(LayoutLog::default()),
        }
    }

    fn begin_frame(&self) {
        self.ids.set(0);
        self.sliders.set(0);
        self.wants_frame.set(false);
        #[cfg(test)]
        self.log.borrow_mut().clear();
    }

    fn finish_frame(&self, repaint: &PollSubNotifier) {
        self.pressed.set(None);
        self.canvas.set(None);
        if self.wants_frame.get() {
            repaint.notify();
        }
    }

    fn alloc(&self) -> u32 {
        let id = self.ids.get();
        self.ids.set(id.wrapping_add(1));
        id
    }

    fn note_slider(&self, readout: Length, track: Length) {
        #[cfg(test)]
        {
            let mut log = self.log.borrow_mut();
            log.readouts.push(readout);
            log.tracks.push(track);
        }
        #[cfg(not(test))]
        let _ = (readout, track);
    }

    fn note_hint(&self) {
        #[cfg(test)]
        {
            self.log.borrow_mut().hint = true;
        }
    }
}

#[cfg(test)]
impl LayoutLog {
    fn clear(&mut self) {
        self.readouts.clear();
        self.tracks.clear();
        self.hint = false;
    }
}

struct Face<'a> {
    chrome: &'a Chrome,
    children: Vec<El<'static>>,
}

impl Face<'_> {
    fn push(&mut self, child: impl Into<El<'static>>) {
        self.children.push(child.into());
    }
}

impl Surface for Face<'_> {
    type Painter<'a>
        = MarkPainter
    where
        Self: 'a;

    fn heading(&mut self, text_value: &str) {
        self.push(
            text(text_value.to_owned())
                .size(HEADING)
                .color(ink(TEXT))
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::Word),
        );
    }

    fn label(&mut self, text_value: &str) {
        self.push(body(text_value));
    }

    fn hint(&mut self, text_value: &str, size: f32) {
        self.chrome.note_hint();
        self.push(
            text(text_value.to_owned())
                .size(size)
                .color(ink(MUTED))
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::Word),
        );
    }

    fn space(&mut self, amount: f32) {
        self.push(
            iced::widget::Space::new()
                .width(Length::Fill)
                .height(amount),
        );
    }

    fn button(&mut self, label: &str, enabled: bool) -> bool {
        let id = self.chrome.alloc();
        let pressed = enabled && self.chrome.pressed.get() == Some(id);
        let mut widget = button(body(label).width(Length::Shrink))
            .padding([4.0, 10.0])
            .style(move |_theme, _status| button_look(enabled));
        if enabled {
            widget = widget.on_press(Message::Press(id));
        }
        self.push(widget);
        pressed
    }

    fn row(&mut self, body: impl FnOnce(&mut Self)) {
        let mut nested = Face {
            chrome: self.chrome,
            children: Vec::new(),
        };
        body(&mut nested);
        self.push(
            row(nested.children)
                .spacing(ROW_GAP)
                .width(Length::Fill)
                .align_y(iced::alignment::Vertical::Center),
        );
    }

    fn split_two(&mut self, body: impl FnOnce(&mut Self, &mut Self)) {
        let mut left = Face {
            chrome: self.chrome,
            children: Vec::new(),
        };
        let mut right = Face {
            chrome: self.chrome,
            children: Vec::new(),
        };
        body(&mut left, &mut right);
        self.push(
            row([left.into_column(), right.into_column()])
                .spacing(ROW_GAP)
                .width(Length::Fill),
        );
    }

    fn text_field(&mut self, value: &mut String, hint: &str, width: f32) {
        let shown = {
            let mut edit = self.chrome.path.borrow_mut();
            let mut seen = self.chrome.path_seen.borrow_mut();
            if value != &*seen {
                edit.clone_from(value);
                seen.clone_from(value);
            } else if *edit != *value {
                value.clone_from(&edit);
                seen.clone_from(value);
            }
            edit.clone()
        };
        self.push(
            text_input(hint, &shown)
                .on_input(Message::Path)
                .width(Length::Fixed(width)),
        );
    }

    fn param_slider<P: Param>(&mut self, label: &str, param: &P, _setter: &ParamSetter<'_>) {
        let index = self.chrome.sliders.get();
        self.chrome.sliders.set(index.wrapping_add(1));
        let readout = Length::Fixed(READOUT_WIDTH);
        let track = Length::Fill;
        self.chrome.note_slider(readout, track);
        let fraction = param.modulated_normalized_value();
        self.push(
            column([
                body(label).into(),
                row([
                    iced::widget::canvas(Track {
                        index: index as u8,
                        fraction,
                    })
                    .width(track)
                    .height(Length::Fixed(TRACK_HEIGHT))
                    .into(),
                    text(param.to_string())
                        .size(BODY)
                        .color(ink(TEXT))
                        .width(readout)
                        .align_x(iced::alignment::Horizontal::Right)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .into(),
                ])
                .spacing(ROW_GAP)
                .width(Length::Fill)
                .align_y(iced::alignment::Vertical::Center)
                .into(),
            ])
            .spacing(2.0)
            .width(Length::Fill),
        );
    }

    fn progress(&mut self, fraction: f32, text_value: &str) {
        let mut painter = MarkPainter::default();
        let rect = Rect::from_min_size(Point { x: 0.0, y: 0.0 }, VIRTUAL_WIDTH, METER_HEIGHT);
        painter.fill(rect, 4.0, TRACK);
        let width = (fraction.clamp(0.0, 1.0) * VIRTUAL_WIDTH).max(0.0);
        if width >= 0.5 {
            painter.fill(
                Rect::from_min_size(Point { x: 0.0, y: 0.0 }, width, METER_HEIGHT),
                4.0,
                TEAL,
            );
        }
        painter.finish();
        self.push(
            stack([
                marks_canvas(painter.marks, VIRTUAL_WIDTH, METER_HEIGHT, None).into(),
                text(text_value.to_owned())
                    .size(BODY)
                    .color(ink(TEXT))
                    .width(Length::Fill)
                    .height(Length::Fixed(METER_HEIGHT))
                    .align_x(iced::alignment::Horizontal::Center)
                    .align_y(iced::alignment::Vertical::Center)
                    .into(),
            ])
            .width(Length::Fill)
            .height(Length::Fixed(METER_HEIGHT)),
        );
    }

    fn canvas<'a, R>(
        &'a mut self,
        breadth: Breadth,
        height: f32,
        sense: Sense,
        draw: impl FnOnce(Rect, &mut Self::Painter<'a>, Option<Point>) -> R,
    ) -> R {
        let virtual_w = match breadth {
            Breadth::Fixed(width) => width,
            Breadth::Fill => VIRTUAL_WIDTH,
        };
        let id = match sense {
            Sense::Click => Some(self.chrome.alloc()),
            Sense::Hover => None,
        };
        let click = id.and_then(|id| {
            self.chrome
                .canvas
                .get()
                .and_then(|(stored, x, y)| (stored == id).then_some(Point { x, y }))
        });
        let rect = Rect::from_min_size(Point { x: 0.0, y: 0.0 }, virtual_w, height);
        let mut painter = MarkPainter::default();
        let result = draw(rect, &mut painter, click);
        painter.finish();
        let mut widget = marks_canvas(painter.marks, virtual_w, height, id);
        widget = match breadth {
            Breadth::Fixed(width) => widget.width(Length::Fixed(width)),
            Breadth::Fill => widget.width(Length::Fill),
        };
        self.push(widget.height(Length::Fixed(height)));
        result
    }

    fn request_repaint(&mut self) {
        self.chrome.wants_frame.set(true);
    }
}

impl Face<'_> {
    fn into_column(self) -> El<'static> {
        column(self.children)
            .spacing(COLUMN_GAP)
            .width(Length::Fill)
            .into()
    }
}

fn body(value: impl Into<String>) -> iced::widget::Text<'static, Theme, Renderer> {
    text(value.into())
        .size(BODY)
        .color(ink(TEXT))
        .width(Length::Fill)
        .wrapping(iced::widget::text::Wrapping::Word)
}

fn button_look(enabled: bool) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(iced::Background::Color(ink(if enabled {
            BUTTON
        } else {
            BUTTON_OFF
        }))),
        text_color: ink(if enabled { TEXT } else { MUTED }),
        ..iced::widget::button::Style::default()
    }
}

fn paper(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        text_color: Some(ink(TEXT)),
        background: Some(iced::Background::Color(ink(PAPER))),
        ..iced::widget::container::Style::default()
    }
}

fn ink(color: Color) -> iced::Color {
    // iced stores sRGB and its shader multiplies rgb by alpha. Pass straight
    // bytes. Decoding or premultiplying here would darken the lane wash.
    iced::Color::from_rgba8(color.r, color.g, color.b, color.a as f32 / 255.0)
}

fn scale_x(virtual_w: f32, bounds_w: f32, x: f32) -> f32 {
    if virtual_w > 0.0 {
        x * bounds_w / virtual_w
    } else {
        x
    }
}

fn unscale_x(virtual_w: f32, bounds_w: f32, local_x: f32) -> f32 {
    if bounds_w > 0.0 {
        local_x * virtual_w / bounds_w
    } else {
        local_x
    }
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

enum Mark {
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

#[derive(Default)]
struct MarkPainter {
    marks: Vec<Mark>,
    lines: Option<LineBatch>,
}

struct LineBatch {
    width: f32,
    color: Color,
    segments: Vec<[f32; 4]>,
}

impl MarkPainter {
    fn finish(&mut self) {
        self.flush_lines();
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
                segments: Vec::new(),
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
}

struct Marks {
    marks: Vec<Mark>,
    virtual_w: f32,
    id: Option<u32>,
}

#[derive(Default)]
struct Arm {
    armed: bool,
}

fn marks_canvas(
    marks: Vec<Mark>,
    virtual_w: f32,
    height: f32,
    id: Option<u32>,
) -> canvas::Canvas<Marks, Message, Theme, Renderer> {
    let _ = height;
    iced::widget::canvas(Marks {
        marks,
        virtual_w,
        id,
    })
}

impl canvas::Program<Message, Theme, Renderer> for Marks {
    type State = Arm;

    fn update(
        &self,
        state: &mut Arm,
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
        _state: &Arm,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        for mark in &self.marks {
            match mark {
                Mark::Fill {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    color,
                } => {
                    let x = scale_x(self.virtual_w, bounds.width, *x);
                    let w = scale_x(self.virtual_w, bounds.width, *w);
                    fill_round(&mut frame, x, *y, w, *h, *radius, *color);
                }
                Mark::Lines {
                    width,
                    color,
                    segments,
                } => {
                    let path = Path::new(|builder| {
                        for segment in segments {
                            builder.move_to(iced::Point::new(
                                scale_x(self.virtual_w, bounds.width, segment[0]),
                                segment[1],
                            ));
                            builder.line_to(iced::Point::new(
                                scale_x(self.virtual_w, bounds.width, segment[2]),
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
                        position: iced::Point::new(scale_x(self.virtual_w, bounds.width, *x), *y),
                        max_width: bounds.width,
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
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Arm,
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

fn fill_round(
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

struct Track {
    index: u8,
    fraction: f32,
}

#[derive(Default)]
struct TrackState {
    dragging: bool,
    previous: Option<iced::core::mouse::Click>,
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
                            index: self.index,
                        }))
                        .and_capture(),
                    );
                }
                state.dragging = true;
                Some(
                    canvas::Action::publish(Message::Slider(SliderEvent::Down {
                        index: self.index,
                        x: position.x - bounds.x,
                        width: bounds.width,
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
                        index: self.index,
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
                        index: self.index,
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
                    canvas::Action::publish(Message::Slider(SliderEvent::Cancel {
                        index: self.index,
                    }))
                    .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &TrackState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let radius = TRACK_HEIGHT * 0.5;
        fill_round(
            &mut frame,
            0.0,
            0.0,
            bounds.width,
            bounds.height,
            radius,
            TRACK,
        );
        let fill_w = (self.fraction.clamp(0.0, 1.0) * bounds.width).max(0.0);
        if fill_w >= 0.5 {
            fill_round(&mut frame, 0.0, 0.0, fill_w, bounds.height, radius, TEAL);
        }
        vec![frame.into_geometry()]
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

fn apply_slider(gui: &GuiContext, params: &SonomoriParams, chrome: &Chrome, event: SliderEvent) {
    match event {
        SliderEvent::Down { index, x, width: _ } => {
            if chrome.command.get() {
                with_knob(gui, params, index, |knob, setter| {
                    knob.begin(setter);
                    knob.reset(setter);
                    knob.end(setter);
                });
                chrome.drag.set(None);
                return;
            }
            let start = normalized(params, index);
            chrome.drag.set(Some(DragMem {
                index,
                origin_norm: start,
                origin_x: x,
                shift: chrome.shift.get(),
                start_norm: start,
            }));
            with_knob(gui, params, index, |knob, setter| knob.begin(setter));
        }
        SliderEvent::Move { index, x, width } => {
            drag_to(gui, params, chrome, index, x, width, false);
        }
        SliderEvent::Up { index, x, width } => {
            drag_to(gui, params, chrome, index, x, width, true);
        }
        SliderEvent::Reset { index } => {
            let open = chrome
                .drag
                .get()
                .is_some_and(|memory| memory.index == index);
            with_knob(gui, params, index, |knob, setter| {
                if !open {
                    knob.begin(setter);
                }
                knob.reset(setter);
                if !open {
                    knob.end(setter);
                }
            });
        }
        SliderEvent::Cancel { index } => {
            let Some(memory) = chrome.drag.get() else {
                return;
            };
            if memory.index != index {
                return;
            }
            with_knob(gui, params, index, |knob, setter| {
                knob.restore(setter, memory.start_norm);
                knob.end(setter);
            });
            chrome.drag.set(None);
        }
    }
}

fn drag_to(
    gui: &GuiContext,
    params: &SonomoriParams,
    chrome: &Chrome,
    index: u8,
    x: f32,
    width: f32,
    finish: bool,
) {
    let Some(mut memory) = chrome.drag.get() else {
        return;
    };
    if memory.index != index {
        return;
    }
    let shift = chrome.shift.get();
    if shift != memory.shift {
        if shift {
            memory.origin_norm = normalized(params, index);
            memory.origin_x = x;
        }
        memory.shift = shift;
    }
    let norm = drag_norm(memory.origin_norm, memory.origin_x, x, width, memory.shift);
    with_knob(gui, params, index, |knob, setter| {
        knob.set_norm(setter, norm);
    });
    if finish {
        with_knob(gui, params, index, |knob, setter| knob.end(setter));
        chrome.drag.set(None);
    } else {
        chrome.drag.set(Some(memory));
    }
}

fn normalized(params: &SonomoriParams, index: u8) -> f32 {
    knob(params, index)
        .map(|knob| knob.normalized())
        .unwrap_or(0.0)
}

fn with_knob(
    gui: &GuiContext,
    params: &SonomoriParams,
    index: u8,
    body: impl FnOnce(Knob<'_>, &ParamSetter<'_>),
) {
    let Some(knob) = knob(params, index) else {
        return;
    };
    let setter = gui.param_setter();
    body(knob, &setter);
}

#[derive(Clone, Copy)]
enum Knob<'a> {
    Float(&'a FloatParam),
    Int(&'a IntParam),
}

impl Knob<'_> {
    fn normalized(self) -> f32 {
        match self {
            Knob::Float(param) => param.modulated_normalized_value(),
            Knob::Int(param) => param.modulated_normalized_value(),
        }
    }

    fn begin(self, setter: &ParamSetter<'_>) {
        match self {
            Knob::Float(param) => setter.begin_set_parameter(param),
            Knob::Int(param) => setter.begin_set_parameter(param),
        }
    }

    fn end(self, setter: &ParamSetter<'_>) {
        match self {
            Knob::Float(param) => setter.end_set_parameter(param),
            Knob::Int(param) => setter.end_set_parameter(param),
        }
    }

    fn set_norm(self, setter: &ParamSetter<'_>, norm: f32) {
        match self {
            Knob::Float(param) => set_plain(setter, param, norm),
            Knob::Int(param) => set_plain(setter, param, norm),
        }
    }

    fn reset(self, setter: &ParamSetter<'_>) {
        match self {
            Knob::Float(param) => setter.set_parameter(param, param.default_plain_value()),
            Knob::Int(param) => setter.set_parameter(param, param.default_plain_value()),
        }
    }

    fn restore(self, setter: &ParamSetter<'_>, norm: f32) {
        match self {
            Knob::Float(param) => setter.set_parameter_normalized(param, norm),
            Knob::Int(param) => setter.set_parameter_normalized(param, norm),
        }
    }
}

fn set_plain<P: Param>(setter: &ParamSetter<'_>, param: &P, norm: f32)
where
    P::Plain: PartialEq,
{
    let plain = param.preview_plain(norm);
    if plain != param.modulated_plain_value() {
        setter.set_parameter(param, plain);
    }
}

fn knob(params: &SonomoriParams, index: u8) -> Option<Knob<'_>> {
    Some(match index {
        0 => Knob::Float(&params.density),
        1 => Knob::Float(&params.syncopation),
        2 => Knob::Float(&params.symmetry),
        3 => Knob::Float(&params.pattern_var),
        4 => Knob::Float(&params.swing),
        5 => Knob::Int(&params.bars),
        6 => Knob::Float(&params.sound_var),
        7 => Knob::Float(&params.decay),
        8 => Knob::Int(&params.pitch),
        9 => Knob::Float(&params.tone),
        10 => Knob::Float(&params.sensitivity),
        11 => Knob::Float(&params.tempo),
        12 => Knob::Float(&params.output),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use nice_plug::context::gui::GuiContext;
    use nice_plug::params::InternalParamMut;
    use nice_plug::prelude::{GuiContextInner, ParamPtr, PluginApi, PluginState};

    use nice_plug::prelude::Param;
    use nice_plug_iced::iced::Length;

    use crate::editor::SonomoriEditor;
    use crate::params::SonomoriParams;
    use crate::session::{Shared, Snapshot};

    use super::{Chrome, SliderEvent, apply_slider, drag_norm, scale_x, unscale_x};

    fn render_editor(editor: &mut SonomoriEditor, chrome: &Chrome) {
        chrome.begin_frame();
        let gui = editor.gui();
        let mut face = super::Face {
            chrome,
            children: Vec::new(),
        };
        if let Some(gui) = gui {
            let setter = gui.param_setter();
            editor.show(&mut face, &setter);
        }
        let _ = face.children;
    }

    struct DummyGui {
        params: Arc<SonomoriParams>,
        begins: AtomicU32,
        ends: AtomicU32,
    }

    impl GuiContextInner for DummyGui {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Standalone
        }
        unsafe fn raw_begin_set_parameter(&self, _: ParamPtr) {
            self.begins.fetch_add(1, Ordering::Relaxed);
        }
        unsafe fn raw_set_parameter_normalized(&self, ptr: ParamPtr, normalized: f32) {
            apply(&self.params, ptr, normalized);
        }
        unsafe fn raw_end_set_parameter(&self, _: ParamPtr) {
            self.ends.fetch_add(1, Ordering::Relaxed);
        }
        fn get_state(&self) -> PluginState {
            PluginState {
                version: String::new(),
                params: Default::default(),
                fields: Default::default(),
            }
        }
        fn set_state(&self, _: PluginState) {}
        fn request_restart(&self) {}
    }

    fn apply(params: &SonomoriParams, ptr: ParamPtr, normalized: f32) {
        macro_rules! float {
            ($($field:ident),*) => {$(
                if ptr == params.$field.as_ptr() {
                    let plain = params.$field.preview_plain(normalized);
                    unsafe { params.$field._internal_set_plain_value(plain); }
                    return;
                }
            )*};
        }
        macro_rules! int {
            ($($field:ident),*) => {$(
                if ptr == params.$field.as_ptr() {
                    let plain = params.$field.preview_plain(normalized);
                    unsafe { params.$field._internal_set_plain_value(plain); }
                    return;
                }
            )*};
        }
        float!(
            density,
            syncopation,
            symmetry,
            pattern_var,
            swing,
            sound_var,
            decay,
            tone,
            sensitivity,
            tempo,
            output
        );
        int!(pitch, bars, seed);
    }

    struct Fixture {
        editor: SonomoriEditor,
        chrome: Chrome,
        params: Arc<SonomoriParams>,
        dummy: Arc<DummyGui>,
        gui: GuiContext,
    }

    fn fixture() -> Fixture {
        let params = Arc::new(SonomoriParams::default());
        let shared = Shared::new();
        let (preview_tx, _rx) = rtrb::RingBuffer::new(8);
        let (_snap_in, snapshot) = triple_buffer::triple_buffer(&Snapshot::default());
        let mut editor = SonomoriEditor::new(params.clone(), shared, preview_tx, snapshot);
        let dummy = Arc::new(DummyGui {
            params: params.clone(),
            begins: AtomicU32::new(0),
            ends: AtomicU32::new(0),
        });
        let gui = GuiContext::new(dummy.clone());
        editor.attach_gui(gui.clone());
        Fixture {
            editor,
            chrome: Chrome::new(),
            params,
            dummy,
            gui,
        }
    }

    impl Fixture {
        fn slide(&self, event: SliderEvent) {
            apply_slider(&self.gui, &self.params, &self.chrome, event);
        }
    }

    #[test]
    fn editor_fits_and_sliders_do_not_move_each_other() {
        let mut fx = fixture();
        render_editor(&mut fx.editor, &fx.chrome);
        let log = fx.chrome.log.borrow();
        assert_eq!(log.readouts.len(), 13);
        assert!(log.hint, "the hint was not built");
        assert!(
            log.readouts
                .iter()
                .all(|length| *length == Length::Fixed(super::READOUT_WIDTH)),
            "a readout width follows the current value"
        );
        assert!(
            log.tracks.iter().all(|length| *length == Length::Fill),
            "a track does not fill the space beside the readout"
        );
        drop(log);

        let floats = [
            fx.params.symmetry.value(),
            fx.params.pattern_var.value(),
            fx.params.swing.value(),
            fx.params.decay.value(),
            fx.params.tone.value(),
            fx.params.sensitivity.value(),
            fx.params.tempo.value(),
            fx.params.output.value(),
        ];
        let ints = [
            fx.params.pitch.value(),
            fx.params.bars.value(),
            fx.params.seed.value(),
        ];
        for (density, sound, sync) in [
            (1.0, 1.0, 0.0),
            (1.0, 0.25, -0.04),
            (0.62, 0.25, 0.5),
            (0.09, 1.0, 1.0),
            (0.62, 0.25, -1.0),
        ] {
            unsafe {
                fx.params.density._internal_set_plain_value(density);
                fx.params.sound_var._internal_set_plain_value(sound);
                fx.params.syncopation._internal_set_plain_value(sync);
            }
            render_editor(&mut fx.editor, &fx.chrome);
            let log = fx.chrome.log.borrow();
            assert!(
                log.readouts
                    .iter()
                    .all(|length| *length == Length::Fixed(super::READOUT_WIDTH)),
                "moving density={density} sound={sound} sync={sync} resized a readout"
            );
            assert!(log.tracks.iter().all(|length| *length == Length::Fill));
        }
        assert_eq!(
            [
                fx.params.symmetry.value(),
                fx.params.pattern_var.value(),
                fx.params.swing.value(),
                fx.params.decay.value(),
                fx.params.tone.value(),
                fx.params.sensitivity.value(),
                fx.params.tempo.value(),
                fx.params.output.value(),
            ],
            floats
        );
        assert_eq!(
            [
                fx.params.pitch.value(),
                fx.params.bars.value(),
                fx.params.seed.value()
            ],
            ints
        );
    }

    #[test]
    fn dragging_density_leaves_the_other_parameters() {
        let fx = fixture();
        let before = fx.params.density.value();
        let others = fx.params.symmetry.value();
        let width = 200.0;
        let x = width * 0.2;
        fx.slide(SliderEvent::Down { index: 0, x, width });
        fx.slide(SliderEvent::Up { index: 0, x, width });
        let density = fx.params.density.value();
        assert!(
            (density - before).abs() > 0.05,
            "the drag left density at {density}"
        );
        assert!(
            (density - 0.2).abs() < 0.08,
            "a click at 20% set density to {density}"
        );
        assert_eq!(fx.params.symmetry.value(), others);
        assert_eq!(fx.params.seed.value(), 1);
        assert_eq!(fx.dummy.begins.load(Ordering::Relaxed), 1);
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn shift_drag_stays_near_the_current_value() {
        let fx = fixture();
        fx.chrome.shift.set(true);
        let start = fx.params.density.value();
        fx.slide(SliderEvent::Down {
            index: 0,
            x: 100.0,
            width: 400.0,
        });
        fx.slide(SliderEvent::Move {
            index: 0,
            x: 110.0,
            width: 400.0,
        });
        let density = fx.params.density.value();
        assert!(
            (density - start).abs() < 0.05,
            "shift-drag jumped density from {start} to {density}"
        );
        assert!(
            (density - start).abs() > 0.005,
            "shift-drag left density at {density}"
        );
        assert!(
            (density - 110.0 / 400.0).abs() > 0.2,
            "shift-drag followed the pointer to {density}"
        );
        assert_eq!(
            fx.params.symmetry.value(),
            fx.params.symmetry.default_plain_value()
        );
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 0);
        fx.slide(SliderEvent::Up {
            index: 0,
            x: 110.0,
            width: 400.0,
        });
        assert_eq!(fx.dummy.begins.load(Ordering::Relaxed), 1);
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn command_click_resets_and_closes_the_gesture() {
        let fx = fixture();
        unsafe { fx.params.density._internal_set_plain_value(1.0) };
        fx.chrome.command.set(true);
        fx.slide(SliderEvent::Down {
            index: 0,
            x: 10.0,
            width: 200.0,
        });
        assert_eq!(
            fx.params.density.value(),
            fx.params.density.default_plain_value()
        );
        assert!(fx.chrome.drag.get().is_none());
        fx.slide(SliderEvent::Up {
            index: 0,
            x: 180.0,
            width: 200.0,
        });
        assert_eq!(
            fx.params.density.value(),
            fx.params.density.default_plain_value()
        );
        assert_eq!(fx.dummy.begins.load(Ordering::Relaxed), 1);
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 1);
        assert_eq!(fx.params.seed.value(), 1);
    }

    #[test]
    fn escape_restores_the_value_from_the_press() {
        let fx = fixture();
        let start = fx.params.density.value();
        fx.slide(SliderEvent::Down {
            index: 0,
            x: 40.0,
            width: 200.0,
        });
        fx.slide(SliderEvent::Move {
            index: 0,
            x: 200.0,
            width: 200.0,
        });
        assert!((fx.params.density.value() - start).abs() > 0.2);
        fx.slide(SliderEvent::Cancel { index: 0 });
        assert_eq!(fx.params.density.value(), start);
        assert!(fx.chrome.drag.get().is_none());
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn granular_step_is_a_fraction_of_a_pixel() {
        let norm = drag_norm(0.62, 100.0, 110.0, 400.0, true);
        assert!((norm - (0.62 + 10.0 * super::GRANULAR_DRAG)).abs() < 1.0e-6);
        assert!((drag_norm(0.62, 100.0, 80.0, 200.0, false) - 0.4).abs() < 1.0e-6);
    }

    #[test]
    fn marks_scale_x_and_keep_a_click_in_virtual_space() {
        let virtual_w = 1000.0;
        let bounds_w = 760.0;
        let x = 250.0;
        let pixel = scale_x(virtual_w, bounds_w, x);
        assert!((pixel - 190.0).abs() < 1.0e-4);
        assert!((unscale_x(virtual_w, bounds_w, pixel) - x).abs() < 1.0e-3);
        assert!((scale_x(virtual_w, bounds_w, 1.0) - bounds_w / virtual_w).abs() < 1.0e-6);
    }
}
