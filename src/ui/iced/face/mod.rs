//! The editor screen, drawn with iced.
//!
//! `Screen` latches input while iced updates, then the next view reads those
//! latches through [`Surface`]. Waveform and pattern marks are recorded in a
//! virtual width and replayed into one canvas frame, with X scaled to the
//! laid-out width and stroke widths left in pixels. Slider tracks are a
//! separate canvas so a drag that leaves the 14 px track still moves.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nice_plug::prelude::{Param, ParamSetter};
use nice_plug_iced::iced::widget::{button, column, container, lazy, row, stack, text, text_input};
use nice_plug_iced::iced::{
    self, Element, Length, PollSubNotifier, Renderer, Subscription, Task, Theme,
};
use nice_plug_iced::{IcedNiceContext, PersistentState};

use crate::editor::SonomoriEditor;
use crate::ui::{ACCENT, Breadth, Color, MUTED, Painter, Point, Rect, Sense, Surface, TRACK};

mod marks;
mod slider;

use marks::{Mark, MarkPainter, marks_canvas};
use slider::{DragMem, SliderEvent, Track, apply_slider};

const COLUMN_GAP: f32 = 6.0;
const ROW_GAP: f32 = 8.0;
const PADDING: f32 = 8.0;
const READOUT_WIDTH: f32 = 84.0;
const TRACK_HEIGHT: f32 = 14.0;
const METER_HEIGHT: f32 = 16.0;
/// Fill-width canvases are painted in this width. A click is mapped back by
/// the same ratio the frame uses to place marks.
const VIRTUAL_WIDTH: f32 = 1000.0;
const BODY: f32 = 13.0;
const HEADING: f32 = 20.0;

const TEXT: Color = Color::rgb(232, 232, 236);
const PAPER: Color = Color::rgb(24, 24, 27);
const BUTTON: Color = Color::rgb(46, 48, 54);
const BUTTON_OFF: Color = Color::rgb(36, 38, 42);

type El<'a> = Element<'a, Message, Theme, Renderer>;

struct Counters {
    views: AtomicU64,
    view_ns: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    reuse: AtomicU64,
    built: AtomicU64,
    chrome: AtomicU64,
}

static COUNTERS: Counters = Counters {
    views: AtomicU64::new(0),
    view_ns: AtomicU64::new(0),
    hits: AtomicU64::new(0),
    misses: AtomicU64::new(0),
    reuse: AtomicU64::new(0),
    built: AtomicU64::new(0),
    chrome: AtomicU64::new(0),
};

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
    /// Mark buffers kept across frames so a steady loop does not allocate.
    mark_pool: RefCell<Vec<Vec<Mark>>>,
    /// Last cached-mesh fingerprint for each canvas, in paint order.
    slots: RefCell<Vec<u64>>,
    slot_cursor: Cell<usize>,
    input_epoch: Cell<u64>,
    stats_at: Cell<Instant>,
    #[cfg(test)]
    log: RefCell<LayoutLog>,
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
                if let Some(gui) = self.editor.borrow().gui() {
                    apply_slider(&gui, &self.chrome, event);
                }
            }
            Message::Canvas { id, x, y } => self.chrome.canvas.set(Some((id, x, y))),
        }
        Task::none()
    }

    pub(super) fn view(&self) -> El<'_> {
        let started = Instant::now();
        self.chrome.begin_frame();
        let (key, pump) = {
            let mut editor = self.editor.borrow_mut();
            editor.prepare();
            let base = editor.chrome_key();
            let path_changed = self.chrome.path_differs(editor.path_input());
            let pump = editor.wants_pump();
            drop(editor);
            let pending = path_changed
                || self.chrome.pressed.get().is_some()
                || self.chrome.canvas.get().is_some();
            let key = if pending {
                self.chrome.bump(base)
            } else {
                base
            };
            (key, pump)
        };
        if pump {
            // Loading and the file panel need another look. Playback does not:
            // the audio thread wakes the editor when the playhead actually moves.
            self.repaint.notify();
        }

        // The waveform and the sliders do not change on a playhead step. `lazy`
        // keeps their widget trees, so a step only rebuilds the grid and the meter.
        let top = lazy(key, |_| self.rebuild_top());
        let sliders = lazy(key, |_| self.rebuild_sliders());
        let pattern = self.with_editor(|editor, face| editor.paint_grid(face));
        let readout = self.with_editor(|editor, face| editor.paint_readout(face));

        let mut children = Vec::with_capacity(pattern.len() + readout.len() + 2);
        children.push(top.into());
        children.extend(pattern);
        children.push(sliders.into());
        children.extend(readout);

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
        self.note_frame(started);
        root.into()
    }

    fn rebuild_top(&self) -> El<'static> {
        COUNTERS.chrome.fetch_add(1, Ordering::Relaxed);
        let mut editor = self.editor.borrow_mut();
        let gui = editor.gui();
        let mut face = Face {
            chrome: &self.chrome,
            children: Vec::new(),
        };
        if let Some(gui) = gui {
            let setter = gui.param_setter();
            editor.paint_top(&mut face, &setter);
        }
        drop(editor);
        self.chrome.end_input();
        if self.chrome.wants_frame.get() {
            self.repaint.notify();
        }
        face.into_column()
    }

    fn rebuild_sliders(&self) -> El<'static> {
        COUNTERS.chrome.fetch_add(1, Ordering::Relaxed);
        let mut editor = self.editor.borrow_mut();
        let gui = editor.gui();
        let mut face = Face {
            chrome: &self.chrome,
            children: Vec::new(),
        };
        if let Some(gui) = gui {
            let setter = gui.param_setter();
            editor.paint_sliders(&mut face, &setter);
        }
        face.into_column()
    }

    fn with_editor(
        &self,
        body: impl FnOnce(&mut crate::editor::SonomoriEditor, &mut Face<'_>),
    ) -> Vec<El<'static>> {
        let mut editor = self.editor.borrow_mut();
        let mut face = Face {
            chrome: &self.chrome,
            children: Vec::new(),
        };
        body(&mut editor, &mut face);
        face.children
    }

    fn note_frame(&self, started: Instant) {
        COUNTERS.views.fetch_add(1, Ordering::Relaxed);
        COUNTERS
            .view_ns
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if self.chrome.stats_at.get().elapsed() < Duration::from_secs(1) {
            return;
        }
        self.chrome.stats_at.set(Instant::now());
        write_stats();
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
        write_stats();
    }
}

/// Cumulative frame counters for the profile script. Absent unless
/// `SONOMORI_STATS` is set, and rewritten at most once a second.
fn write_stats() {
    let Some(path) = crate::profile::stats_path() else {
        return;
    };
    let views = COUNTERS.views.load(Ordering::Relaxed);
    let nanos = COUNTERS.view_ns.load(Ordering::Relaxed);
    let mean = if views == 0 { 0 } else { nanos / views };
    let body = format!(
        "views {views}\nview_ns_mean {mean}\ngeom_hits {}\ngeom_misses {}\nmark_reuse {}\nmark_build {}\nchrome_rebuilds {}\n",
        COUNTERS.hits.load(Ordering::Relaxed),
        COUNTERS.misses.load(Ordering::Relaxed),
        COUNTERS.reuse.load(Ordering::Relaxed),
        COUNTERS.built.load(Ordering::Relaxed),
        COUNTERS.chrome.load(Ordering::Relaxed),
    );
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
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
            mark_pool: RefCell::new(Vec::new()),
            slots: RefCell::new(Vec::new()),
            slot_cursor: Cell::new(0),
            input_epoch: Cell::new(0),
            stats_at: Cell::new(Instant::now()),
            #[cfg(test)]
            log: RefCell::new(LayoutLog::default()),
        }
    }

    fn begin_frame(&self) {
        self.ids.set(0);
        self.slot_cursor.set(0);
        self.wants_frame.set(false);
        #[cfg(test)]
        self.log.borrow_mut().clear();
    }

    fn finish_frame(&self, repaint: &PollSubNotifier) {
        if self.wants_frame.get() {
            repaint.notify();
        }
    }

    fn end_input(&self) {
        self.pressed.set(None);
        self.canvas.set(None);
    }

    fn path_differs(&self, path: &str) -> bool {
        *self.path.borrow() != path
    }

    fn bump(&self, base: u64) -> u64 {
        let epoch = self.input_epoch.get().wrapping_add(1);
        self.input_epoch.set(epoch);
        mix(base, epoch)
    }

    fn rent_marks(&self) -> Vec<Mark> {
        self.mark_pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(40))
    }

    fn recycle_marks(&self, mut marks: Vec<Mark>) {
        marks.clear();
        self.mark_pool.borrow_mut().push(marks);
    }

    /// Keep the tessellated mesh when this canvas paints the same base marks.
    /// The returned vec is empty on a hit so the caller does not retain a copy.
    fn resolve_marks(&self, fingerprint: u64, marks: Vec<Mark>) -> Vec<Mark> {
        let index = self.slot_cursor.get();
        self.slot_cursor.set(index + 1);
        let mut slots = self.slots.borrow_mut();
        if slots.len() > index && slots[index] == fingerprint {
            drop(slots);
            self.recycle_marks(marks);
            COUNTERS.reuse.fetch_add(1, Ordering::Relaxed);
            return Vec::new();
        }
        if slots.len() == index {
            slots.push(fingerprint);
        } else {
            slots[index] = fingerprint;
        }
        COUNTERS.built.fetch_add(1, Ordering::Relaxed);
        marks
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
        let readout = Length::Fixed(READOUT_WIDTH);
        let track = Length::Fill;
        self.chrome.note_slider(readout, track);
        let fraction = param.modulated_normalized_value();
        self.push(
            column([
                body(label).into(),
                row([
                    iced::widget::canvas(Track {
                        ptr: param.as_ptr(),
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
        let mut painter = MarkPainter::new(self.chrome.rent_marks());
        let rect = Rect::from_min_size(Point { x: 0.0, y: 0.0 }, VIRTUAL_WIDTH, METER_HEIGHT);
        painter.fill(rect, 4.0, TRACK);
        let width = (fraction.clamp(0.0, 1.0) * VIRTUAL_WIDTH).max(0.0);
        if width >= 0.5 {
            painter.fill(
                Rect::from_min_size(Point { x: 0.0, y: 0.0 }, width, METER_HEIGHT),
                4.0,
                ACCENT,
            );
        }
        painter.finish();
        let fingerprint = mix(painter.fingerprint, METER_HEIGHT.to_bits() as u64);
        let overlay_fp = painter.overlay_fp;
        let marks = self.chrome.resolve_marks(fingerprint, painter.marks);
        self.push(
            stack([
                marks_canvas(marks, None, VIRTUAL_WIDTH, None, fingerprint, overlay_fp).into(),
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
        let mut painter = MarkPainter::new(self.chrome.rent_marks());
        let result = draw(rect, &mut painter, click);
        painter.finish();
        let mut fingerprint = mix(painter.fingerprint, virtual_w.to_bits() as u64);
        fingerprint = mix(fingerprint, height.to_bits() as u64);
        let overlay_fp = painter.overlay_fp;
        let overlay = painter.overlay.take();
        let marks = self.chrome.resolve_marks(fingerprint, painter.marks);
        let mut widget = marks_canvas(marks, overlay, virtual_w, id, fingerprint, overlay_fp);
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

const MARK_HASH: u64 = 0xcbf29ce484222325;
const MARK_PRIME: u64 = 0x100000001b3;

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(MARK_PRIME)
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

    use super::Chrome;
    use super::marks::{scale_x, unscale_x};
    use super::slider::{SliderEvent, apply_slider};

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
            apply_slider(&self.gui, &self.chrome, event);
        }

        fn density(&self) -> ParamPtr {
            self.params.density.as_ptr()
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
        let ptr = fx.density();
        let width = 200.0;
        let x = width * 0.2;
        fx.slide(SliderEvent::Down { ptr, x });
        fx.slide(SliderEvent::Up { ptr, x, width });
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
        let ptr = fx.density();
        fx.slide(SliderEvent::Down { ptr, x: 100.0 });
        fx.slide(SliderEvent::Move {
            ptr,
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
            ptr,
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
        let ptr = fx.density();
        fx.slide(SliderEvent::Down { ptr, x: 10.0 });
        assert_eq!(
            fx.params.density.value(),
            fx.params.density.default_plain_value()
        );
        assert!(fx.chrome.drag.get().is_none());
        fx.slide(SliderEvent::Up {
            ptr,
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
        let ptr = fx.density();
        fx.slide(SliderEvent::Down { ptr, x: 40.0 });
        fx.slide(SliderEvent::Move {
            ptr,
            x: 200.0,
            width: 200.0,
        });
        assert!((fx.params.density.value() - start).abs() > 0.2);
        fx.slide(SliderEvent::Cancel { ptr });
        assert_eq!(fx.params.density.value(), start);
        assert!(fx.chrome.drag.get().is_none());
        assert_eq!(fx.dummy.ends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn highlight_stays_out_of_the_cached_fingerprint() {
        use crate::ui::{Color, Painter, Point, Rect};

        let rect = Rect::from_min_size(Point { x: 0.0, y: 0.0 }, 10.0, 8.0);
        let mut plain = super::marks::MarkPainter::default();
        plain.fill(rect, 2.0, Color::rgb(38, 40, 46));
        plain.finish();
        let mut marked = super::marks::MarkPainter::default();
        marked.fill(rect, 2.0, Color::rgb(38, 40, 46));
        marked.highlight(rect.shrink(1.0), 1.0, Color::rgba(255, 255, 255, 70));
        marked.finish();
        assert_eq!(plain.fingerprint, marked.fingerprint);
        assert!(plain.overlay.is_none());
        assert!(marked.overlay.is_some());
    }

    #[test]
    fn marks_scale_x_and_keep_a_click_in_virtual_space() {
        let virtual_w = 1000.0;
        let bounds_w = 760.0;
        let x = 250.0;
        let pixel = scale_x(virtual_w, bounds_w, x);
        assert!(
            pixel < x,
            "a mark in the wider virtual space compresses into the widget"
        );
        assert!((unscale_x(virtual_w, bounds_w, pixel) - x).abs() < 1.0e-3);
    }
}
