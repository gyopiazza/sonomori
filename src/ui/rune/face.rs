//! The editor screen, drawn with Rune.
//!
//! `Face` builds elements while the screen holds the editor lock, so a slider
//! gesture clones the host context and drops that lock before it writes a
//! parameter. Waveform and pattern marks go straight into one [`Shape`]. There
//! is no draw-command list, and pattern cells do not spring.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use nice_plug::prelude::{FloatParam, IntParam, Param, ParamPtr, ParamSetter};
use parking_lot::Mutex;
use rune::{
    Align, AppSpec, Cx, Drag, El, LineCap, Phase, Shape, TextEdit, TextStyle, canvas, col,
    edit_access, el, row, text, text_input,
};

use crate::editor::SonomoriEditor;
use crate::params::SonomoriParams;
use crate::ui::{Breadth, Color, Painter, Point, Rect, Sense, Surface};

const COLUMN_GAP: f32 = 6.0;
const ROW_GAP: f32 = 8.0;
const PADDING: f32 = 8.0;
const READOUT_WIDTH: f32 = 84.0;
const TRACK_HEIGHT: f32 = 14.0;
const METER_HEIGHT: f32 = 16.0;
/// Fill-width canvases are painted in this width and stretched to the laid-out
/// element. A click is mapped back by the same ratio.
const VIRTUAL_WIDTH: f32 = 1000.0;
const GRANULAR_DRAG: f32 = 0.0015;
const BODY: TextStyle = TextStyle::new(13.0, 400.0);
const HEADING: TextStyle = TextStyle::new(20.0, 650.0);

const SLIDER_KEYS: [&str; 13] = [
    "slider-0",
    "slider-1",
    "slider-2",
    "slider-3",
    "slider-4",
    "slider-5",
    "slider-6",
    "slider-7",
    "slider-8",
    "slider-9",
    "slider-10",
    "slider-11",
    "slider-12",
];

pub(crate) struct Model {
    pub(crate) editor: Arc<Mutex<SonomoriEditor>>,
    pub(crate) chrome: Chrome,
}

pub(crate) struct Chrome {
    path: RefCell<TextEdit>,
    path_seen: RefCell<String>,
    pressed: Cell<Option<u32>>,
    canvas: Cell<Option<(u32, f32, f32)>>,
    drag: Cell<Option<DragMem>>,
    pub(crate) shift: Cell<bool>,
    pub(crate) command: Cell<bool>,
    pub(crate) wants_frame: Cell<bool>,
    ids: Cell<u32>,
    sliders: Cell<u32>,
}

#[derive(Clone, Copy)]
struct DragMem {
    ptr: ParamPtr,
    origin_norm: f32,
    origin_dx: f32,
    shift: bool,
    start_norm: f32,
}

impl Model {
    pub(crate) fn new(editor: Arc<Mutex<SonomoriEditor>>) -> Self {
        Self {
            editor,
            chrome: Chrome {
                path: RefCell::new(TextEdit::new("")),
                path_seen: RefCell::new(String::new()),
                pressed: Cell::new(None),
                canvas: Cell::new(None),
                drag: Cell::new(None),
                shift: Cell::new(false),
                command: Cell::new(false),
                wants_frame: Cell::new(false),
                ids: Cell::new(0),
                sliders: Cell::new(0),
            },
        }
    }
}

pub(crate) fn spec() -> AppSpec<Model> {
    AppSpec::new(
        "Sonomori",
        (960.0, 960.0),
        || panic!("the window builds the editor state"),
        view,
    )
}

pub(crate) fn view(model: &Model, cx: &mut Cx<Model>) -> El {
    // A stale flag from the previous view must not keep the display link awake.
    model.chrome.ids.set(0);
    model.chrome.sliders.set(0);
    model.chrome.wants_frame.set(false);
    cx.foreground(ink(Color::rgb(232, 232, 236)));

    let mut face = Face {
        chrome: &model.chrome,
        children: Vec::new(),
    };
    let gui = model.editor.lock().gui();
    if let Some(gui) = gui {
        let setter = gui.param_setter();
        model.editor.lock().show(&mut face, &setter);
    }

    let mut root = col()
        .padding(PADDING)
        .gap(COLUMN_GAP)
        .w_full()
        .h_full()
        .fill(ink(Color::rgb(24, 24, 27)))
        .instant_changes();
    for child in face.children {
        root = root.child(child);
    }
    model.chrome.pressed.set(None);
    model.chrome.canvas.set(None);
    root
}

struct Face<'a> {
    chrome: &'a Chrome,
    children: Vec<El>,
}

impl Face<'_> {
    fn push(&mut self, child: El) {
        self.children.push(child);
    }

    fn alloc(&self) -> u32 {
        let id = self.chrome.ids.get();
        self.chrome.ids.set(id.wrapping_add(1));
        id
    }

    fn into_column(self) -> El {
        let mut column = col().gap(COLUMN_GAP).w_full().shrink();
        for child in self.children {
            column = column.child(child);
        }
        column
    }
}

impl Surface for Face<'_> {
    type Painter<'a>
        = ShapePainter
    where
        Self: 'a;

    fn heading(&mut self, text_value: &str) {
        self.push(
            text(text_value)
                .font(HEADING)
                .color(ink(Color::rgb(232, 232, 236)))
                .wrap()
                .w_full()
                .shrink(),
        );
    }

    fn label(&mut self, text_value: &str) {
        self.push(body_text(text_value, Color::rgb(232, 232, 236)));
    }

    fn hint(&mut self, text_value: &str, size: f32) {
        self.push(
            text(text_value)
                .font(TextStyle::new(size, 400.0))
                .color(ink(Color::rgb(140, 144, 150)))
                .wrap()
                .w_full()
                .shrink()
                .key("foot"),
        );
    }

    fn space(&mut self, amount: f32) {
        self.push(el().h(amount).w_full());
    }

    fn button(&mut self, label: &str, enabled: bool) -> bool {
        let id = self.alloc();
        let pressed = enabled && self.chrome.pressed.get() == Some(id);
        let mut widget = text(label)
            .font(BODY)
            .color(ink(if enabled {
                Color::rgb(232, 232, 236)
            } else {
                Color::rgb(140, 144, 150)
            }))
            .px(10.0)
            .py(4.0)
            .fill(ink(if enabled {
                Color::rgb(46, 48, 54)
            } else {
                Color::rgb(36, 38, 42)
            }));
        if enabled {
            widget = widget.on_press(move |model: &mut Model| {
                model.chrome.pressed.set(Some(id));
            });
        }
        // After the handler, so the button role does not take keyboard focus.
        self.push(widget.focusable(false));
        pressed
    }

    fn row(&mut self, body: impl FnOnce(&mut Self)) {
        let mut nested = Face {
            chrome: self.chrome,
            children: Vec::new(),
        };
        body(&mut nested);
        let mut line = row().gap(ROW_GAP).w_full().shrink().items_center();
        for child in nested.children {
            line = line.child(child);
        }
        self.push(line);
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
            row()
                .gap(ROW_GAP)
                .w_full()
                .shrink()
                .child(left.into_column().grow(1.0).shrink())
                .child(right.into_column().grow(1.0).shrink()),
        );
    }

    fn text_field(&mut self, value: &mut String, hint: &str, width: f32) {
        let field = {
            let mut edit = self.chrome.path.borrow_mut();
            let mut seen = self.chrome.path_seen.borrow_mut();
            if value != &*seen {
                edit.replace_text(value.clone());
                *seen = value.clone();
            } else if edit.text != *value {
                value.clone_from(&edit.text);
                *seen = value.clone();
            }
            text_input(&edit, BODY)
                .single_line()
                .placeholder(hint, ink(Color::rgb(140, 144, 150)))
                .w(width)
                .shrink()
                .on_edit(edit_access(|model: &mut Model| model.chrome.path.get_mut()))
        };
        self.push(field);
    }

    fn param_slider<P: Param>(&mut self, label: &str, param: &P, _setter: &ParamSetter<'_>) {
        let index = self.chrome.sliders.get();
        self.chrome.sliders.set(index.wrapping_add(1));
        let key = SLIDER_KEYS.get(index as usize).copied().unwrap_or("slider");
        let ptr = param.as_ptr();
        let fraction = param.modulated_normalized_value();
        let track = canvas(track_shape(fraction))
            .key(key)
            .grow(1.0)
            .h(TRACK_HEIGHT)
            .shrink()
            .instant_changes()
            .on_drag(move |model: &mut Model, drag: &mut Drag, _| on_slider(model, ptr, drag))
            .on_double_press(move |model: &mut Model| on_slider_double(model, ptr))
            .focusable(false);
        self.push(
            col()
                .gap(2.0)
                .w_full()
                .shrink()
                .child(body_text(label, Color::rgb(232, 232, 236)))
                .child(
                    row()
                        .gap(ROW_GAP)
                        .w_full()
                        .shrink()
                        .items_center()
                        .child(track)
                        .child(
                            text(param.to_string())
                                .font(BODY)
                                .color(ink(Color::rgb(232, 232, 236)))
                                .align(Align::End)
                                .w(READOUT_WIDTH)
                                .shrink(),
                        ),
                ),
        );
    }

    fn progress(&mut self, fraction: f32, text_value: &str) {
        self.push(
            el().key("meter")
                .w_full()
                .h(METER_HEIGHT)
                .shrink()
                .instant_changes()
                .child(
                    canvas(meter_shape(fraction))
                        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                        .instant_changes()
                        .focusable(false),
                )
                .child(
                    text(text_value)
                        .font(BODY)
                        .color(ink(Color::rgb(232, 232, 236)))
                        .align(Align::Center)
                        .center()
                        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                        .focusable(false),
                ),
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
            Sense::Click => Some(self.alloc()),
            Sense::Hover => None,
        };
        let click = id.and_then(|id| {
            self.chrome
                .canvas
                .get()
                .and_then(|(stored, x, y)| (stored == id).then_some(Point { x, y }))
        });
        let rect = Rect::from_min_size(Point { x: 0.0, y: 0.0 }, virtual_w, height);
        let mut painter = ShapePainter {
            shape: Shape::new().view_box(virtual_w, height),
            label: None,
        };
        let result = draw(rect, &mut painter, click);

        let mut shell = el().h(height).shrink().instant_changes();
        shell = match breadth {
            Breadth::Fixed(width) => shell.w(width),
            Breadth::Fill => shell.w_full(),
        };
        let mut drawn = canvas(painter.shape)
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .instant_changes();
        if let Some(id) = id {
            drawn = drawn.on_drag(move |model: &mut Model, drag: &mut Drag, _| {
                if drag.phase != Phase::Up || drag.cancelled() || drag.w <= 0.0 {
                    return;
                }
                let x = drag.x / drag.w * virtual_w;
                let y = if drag.h > 0.0 {
                    drag.y / drag.h * height
                } else {
                    drag.y
                };
                model.chrome.canvas.set(Some((id, x, y)));
            });
        }
        shell = shell.child(drawn.focusable(false));
        if let Some(label) = painter.label {
            shell = shell.child(
                text(label.text)
                    .font(TextStyle::new(label.size, 400.0))
                    .color(ink(label.color))
                    .align(Align::Center)
                    .center()
                    .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                    .focusable(false),
            );
        }
        self.push(shell);
        result
    }

    fn request_repaint(&mut self) {
        self.chrome.wants_frame.set(true);
    }
}

fn body_text(value: &str, color: Color) -> El {
    text(value)
        .font(BODY)
        .color(ink(color))
        .wrap()
        .w_full()
        .shrink()
}

struct Label {
    text: String,
    size: f32,
    color: Color,
}

struct ShapePainter {
    shape: Shape,
    label: Option<Label>,
}

impl Painter for ShapePainter {
    fn fill(&mut self, rect: Rect, radius: f32, color: Color) {
        self.shape = rounded(
            std::mem::take(&mut self.shape),
            rect.left(),
            rect.top(),
            rect.width(),
            rect.height(),
            radius,
            ink(color),
        );
    }

    fn line(&mut self, from: Point, to: Point, width: f32, color: Color) {
        self.shape = std::mem::take(&mut self.shape).line(
            [from.x, from.y],
            [to.x, to.y],
            width,
            LineCap::Butt,
            ink(color),
        );
    }

    fn text_centered(&mut self, _at: Point, text_value: &str, size: f32, color: Color) {
        self.label = Some(Label {
            text: text_value.to_owned(),
            size,
            color,
        });
    }
}

fn track_shape(fraction: f32) -> Shape {
    let mut shape = Shape::new().view_box(VIRTUAL_WIDTH, TRACK_HEIGHT);
    shape = rounded(
        shape,
        0.0,
        0.0,
        VIRTUAL_WIDTH,
        TRACK_HEIGHT,
        TRACK_HEIGHT * 0.5,
        ink(Color::rgb(38, 40, 46)),
    );
    let width = (fraction.clamp(0.0, 1.0) * VIRTUAL_WIDTH).max(0.0);
    if width >= 0.5 {
        shape = rounded(
            shape,
            0.0,
            0.0,
            width,
            TRACK_HEIGHT,
            TRACK_HEIGHT * 0.5,
            ink(Color::rgb(86, 196, 184)),
        );
    }
    shape
}

fn meter_shape(fraction: f32) -> Shape {
    let mut shape = Shape::new().view_box(VIRTUAL_WIDTH, METER_HEIGHT);
    shape = rounded(
        shape,
        0.0,
        0.0,
        VIRTUAL_WIDTH,
        METER_HEIGHT,
        4.0,
        ink(Color::rgb(38, 40, 46)),
    );
    let width = (fraction.clamp(0.0, 1.0) * VIRTUAL_WIDTH).max(0.0);
    if width >= 0.5 {
        shape = rounded(
            shape,
            0.0,
            0.0,
            width,
            METER_HEIGHT,
            4.0,
            ink(Color::rgb(86, 196, 184)),
        );
    }
    shape
}

fn rounded(shape: Shape, x: f32, y: f32, w: f32, h: f32, radius: f32, color: rune::Color) -> Shape {
    if w < 0.5 || h < 0.5 {
        return shape;
    }
    let radius = radius.min(w * 0.5).min(h * 0.5);
    if radius < 0.5 {
        return shape.fill(vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]], color);
    }
    let mut points = Vec::with_capacity(16);
    let corners = [
        (
            x + radius,
            y + radius,
            std::f32::consts::PI,
            std::f32::consts::PI * 1.5,
        ),
        (x + w - radius, y + radius, -std::f32::consts::PI * 0.5, 0.0),
        (
            x + w - radius,
            y + h - radius,
            0.0,
            std::f32::consts::PI * 0.5,
        ),
        (
            x + radius,
            y + h - radius,
            std::f32::consts::PI * 0.5,
            std::f32::consts::PI,
        ),
    ];
    for (index, (cx, cy, start, end)) in corners.into_iter().enumerate() {
        let first = if index == 0 { 0 } else { 1 };
        for step in first..=3 {
            let angle = start + (end - start) * (step as f32 / 3.0);
            points.push([cx + radius * angle.cos(), cy + radius * angle.sin()]);
        }
    }
    shape.fill(points, color)
}

fn ink(color: Color) -> rune::Color {
    rune::Color {
        r: rune::color::srgb_decode(color.r as f32 / 255.0),
        g: rune::color::srgb_decode(color.g as f32 / 255.0),
        b: rune::color::srgb_decode(color.b as f32 / 255.0),
        a: color.a as f32 / 255.0,
    }
}

fn on_slider(model: &mut Model, ptr: ParamPtr, drag: &Drag) {
    let shift = model.chrome.shift.get();
    let command = model.chrome.command.get();
    match drag.phase {
        Phase::Down => {
            if command {
                with_knob(model, ptr, |knob, setter| {
                    knob.begin(setter);
                    knob.reset(setter);
                    knob.end(setter);
                });
                model.chrome.drag.set(None);
                return;
            }
            let start = normalized_of(model, ptr);
            model.chrome.drag.set(Some(DragMem {
                ptr,
                origin_norm: start,
                origin_dx: drag.dx,
                shift,
                start_norm: start,
            }));
            with_knob(model, ptr, |knob, setter| knob.begin(setter));
        }
        Phase::Move | Phase::Up => {
            let Some(mut memory) = model.chrome.drag.get() else {
                return;
            };
            if memory.ptr != ptr {
                return;
            }
            if drag.cancelled() {
                with_knob(model, ptr, |knob, setter| {
                    knob.restore(setter, memory.start_norm);
                    knob.end(setter);
                });
                model.chrome.drag.set(None);
                return;
            }
            if shift != memory.shift {
                if shift {
                    memory.origin_norm = normalized_of(model, ptr);
                    memory.origin_dx = drag.dx;
                }
                memory.shift = shift;
            }
            let norm = if memory.shift {
                (memory.origin_norm + (drag.dx - memory.origin_dx) * GRANULAR_DRAG).clamp(0.0, 1.0)
            } else if drag.w > 0.0 {
                (drag.x / drag.w).clamp(0.0, 1.0)
            } else {
                memory.origin_norm
            };
            with_knob(model, ptr, |knob, setter| knob.set_norm(setter, norm));
            if drag.phase == Phase::Up {
                with_knob(model, ptr, |knob, setter| knob.end(setter));
                model.chrome.drag.set(None);
            } else {
                model.chrome.drag.set(Some(memory));
            }
        }
    }
}

fn on_slider_double(model: &mut Model, ptr: ParamPtr) {
    let open = model
        .chrome
        .drag
        .get()
        .is_some_and(|memory| memory.ptr == ptr);
    with_knob(model, ptr, |knob, setter| {
        if !open {
            knob.begin(setter);
        }
        knob.reset(setter);
        if !open {
            knob.end(setter);
        }
    });
}

fn normalized_of(model: &Model, ptr: ParamPtr) -> f32 {
    let editor = model.editor.lock();
    knob(editor.params(), ptr)
        .map(|knob| knob.normalized())
        .unwrap_or(0.0)
}

fn with_knob(model: &Model, ptr: ParamPtr, body: impl FnOnce(Knob<'_>, &ParamSetter<'_>)) {
    let editor = model.editor.lock();
    let Some(gui) = editor.gui() else {
        return;
    };
    let params = Arc::clone(editor.params());
    drop(editor);
    let setter = gui.param_setter();
    let Some(knob) = knob(&params, ptr) else {
        return;
    };
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

fn knob<'a>(params: &'a SonomoriParams, ptr: ParamPtr) -> Option<Knob<'a>> {
    macro_rules! field {
        ($field:ident, $kind:ident) => {
            if ptr == params.$field.as_ptr() {
                return Some(Knob::$kind(&params.$field));
            }
        };
    }
    field!(density, Float);
    field!(syncopation, Float);
    field!(symmetry, Float);
    field!(pattern_var, Float);
    field!(swing, Float);
    field!(bars, Int);
    field!(sound_var, Float);
    field!(decay, Float);
    field!(pitch, Int);
    field!(tone, Float);
    field!(sensitivity, Float);
    field!(tempo, Float);
    field!(output, Float);
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nice_plug::context::gui::GuiContext;
    use nice_plug::params::InternalParamMut;
    use nice_plug::prelude::{GuiContextInner, Param, ParamPtr, PluginApi, PluginState};
    use rune::{Env, Runtime};

    use crate::editor::SonomoriEditor;
    use crate::params::SonomoriParams;
    use crate::session::{Shared, Snapshot};

    use super::{Model, view};

    struct DummyGui {
        params: Arc<SonomoriParams>,
    }

    impl GuiContextInner for DummyGui {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Standalone
        }
        unsafe fn raw_begin_set_parameter(&self, _: ParamPtr) {}
        unsafe fn raw_set_parameter_normalized(&self, ptr: ParamPtr, normalized: f32) {
            apply(&self.params, ptr, normalized);
        }
        unsafe fn raw_end_set_parameter(&self, _: ParamPtr) {}
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

    fn model() -> Model {
        let params = Arc::new(SonomoriParams::default());
        let shared = Shared::new();
        let (preview_tx, _rx) = rtrb::RingBuffer::new(8);
        let (_snap_in, snapshot) = triple_buffer::triple_buffer(&Snapshot::default());
        let mut editor = SonomoriEditor::new(params.clone(), shared, preview_tx, snapshot);
        editor.attach_gui(GuiContext::new(Arc::new(DummyGui {
            params: params.clone(),
        })));
        Model::new(Arc::new(parking_lot::Mutex::new(editor)))
    }

    fn present(width: f32, height: f32) -> Runtime<Model> {
        let mut runtime = Runtime::new(model(), view, (width, height), Env::new(true, 1.0));
        runtime.settle_all();
        runtime.frame(1.0 / 60.0);
        if runtime.presented("foot").is_none() {
            runtime.frame(1.0 / 60.0);
        }
        runtime
    }

    fn tracks(runtime: &Runtime<Model>) -> Vec<[f32; 4]> {
        (0..13)
            .map(|index| {
                runtime
                    .presented(&format!("slider-{index}"))
                    .unwrap_or_else(|| panic!("slider-{index} was not laid out"))
                    .0
            })
            .collect()
    }

    #[test]
    fn editor_fits_and_sliders_do_not_move_each_other() {
        for size in [crate::WINDOW_SIZE, crate::MIN_WINDOW_SIZE] {
            let runtime = present(size.width, size.height);
            let laid = tracks(&runtime);
            assert_eq!(laid.len(), 13);
            let (foot, _) = runtime
                .presented("foot")
                .expect("the hint was not laid out");
            assert!(
                foot[1] + foot[3] <= size.height + 0.5,
                "hint bottom {:.1} exceeds {}",
                foot[1] + foot[3],
                size.height
            );
            for track in &laid {
                assert!(
                    track[0] + track[2] <= size.width + 0.5,
                    "slider right edge {:.1} exceeds {}",
                    track[0] + track[2],
                    size.width
                );
                assert!(
                    track[1] + track[3] <= size.height + 0.5,
                    "slider bottom {:.1} exceeds {}",
                    track[1] + track[3],
                    size.height
                );
            }
            assert!(
                laid[6][0] > laid[0][0] + 40.0,
                "the columns did not sit side by side"
            );
        }

        let mut runtime = present(crate::WINDOW_SIZE.width, crate::WINDOW_SIZE.height);
        let baseline = tracks(&runtime);
        let params = Arc::clone(runtime.state.editor.lock().params());
        let floats = [
            params.symmetry.value(),
            params.pattern_var.value(),
            params.swing.value(),
            params.decay.value(),
            params.tone.value(),
            params.sensitivity.value(),
            params.tempo.value(),
            params.output.value(),
        ];
        let ints = [
            params.pitch.value(),
            params.bars.value(),
            params.seed.value(),
        ];
        for (density, sound, sync) in [
            (1.0, 1.0, 0.0),
            (1.0, 0.25, -0.04),
            (0.62, 0.25, 0.5),
            (0.09, 1.0, 1.0),
            (0.62, 0.25, -1.0),
        ] {
            unsafe {
                params.density._internal_set_plain_value(density);
                params.sound_var._internal_set_plain_value(sound);
                params.syncopation._internal_set_plain_value(sync);
            }
            runtime.settle_all();
            runtime.frame(1.0 / 60.0);
            assert_eq!(
                tracks(&runtime),
                baseline,
                "moving density={density} sound={sound} sync={sync} moved other tracks"
            );
        }
        assert_eq!(
            [
                params.symmetry.value(),
                params.pattern_var.value(),
                params.swing.value(),
                params.decay.value(),
                params.tone.value(),
                params.sensitivity.value(),
                params.tempo.value(),
                params.output.value(),
            ],
            floats
        );
        assert_eq!(
            [
                params.pitch.value(),
                params.bars.value(),
                params.seed.value()
            ],
            ints
        );
    }

    #[test]
    fn dragging_density_leaves_the_other_parameters() {
        let mut runtime = present(crate::WINDOW_SIZE.width, crate::WINDOW_SIZE.height);
        let params = Arc::clone(runtime.state.editor.lock().params());
        let before = params.density.value();
        let others = params.symmetry.value();
        let track = tracks(&runtime)[0];
        let x = track[0] + track[2] * 0.2;
        let y = track[1] + track[3] * 0.5;
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        let density = params.density.value();
        assert!(
            (density - before).abs() > 0.05,
            "the drag left density at {density}"
        );
        assert!(
            (density - 0.2).abs() < 0.08,
            "a click at 20% set density to {density}"
        );
        assert_eq!(params.symmetry.value(), others);
        assert_eq!(params.seed.value(), 1);
    }
}
