use std::sync::Arc;

use nice_plug::context::gui::GuiContext;
use nice_plug::editor::dpi::LogicalSize;
use nice_plug::prelude::{Param, ParamSetter, ResizeHint};
use nice_plug_egui::{
    EguiEditor, EguiEditorState, EguiNiceSettings, NiceEguiApp, RepaintNotifier,
    create_egui_editor, widgets,
};

use crate::editor::SonomoriEditor;
use crate::ui::{Breadth, Color, Painter, Point, Rect, Sense, Surface};

pub type EditorState = EguiEditorState;
pub type Repaint = RepaintNotifier;
pub type PluginEditor = EguiEditor<SonomoriEditor>;

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
    repaint.request_repaint();
}

pub fn open(
    state: Arc<EditorState>,
    repaint: Repaint,
    title: &str,
    resize: ResizeHint,
    editor: SonomoriEditor,
) -> Option<PluginEditor> {
    create_egui_editor(
        state,
        repaint,
        EguiNiceSettings::new()
            .with_tile(title)
            .with_resize_hint(resize),
        editor,
    )
}

impl NiceEguiApp for SonomoriEditor {
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        _frame: &mut nice_plug_egui::Frame,
    ) -> Result<(), nice_plug_egui::baseview::HandlerError> {
        install_skin(&egui_ctx);
        self.attach_gui(nice_gui_ctx);
        Ok(())
    }

    fn editor_closed(&mut self) {
        self.detach_gui();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut nice_plug_egui::Frame) {
        let Some(gui) = self.gui() else {
            return;
        };
        let setter = gui.param_setter();
        self.show(ui, &setter);
    }
}

/// Widest readout the parameters can show. Measured, not the current value,
/// so the track beside it does not resize while dragging.
fn readout_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let widest = ["-24.0 dB", "240 BPM", "-12 st", "-100", "+100", "100%"]
        .into_iter()
        .map(|text| {
            ui.painter()
                .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    widest.ceil() + 1.0
}

fn install_skin(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}

impl Surface for egui::Ui {
    type Painter<'a>
        = egui::Painter
    where
        Self: 'a;

    fn heading(&mut self, text: &str) {
        egui::Ui::heading(self, text);
    }

    fn label(&mut self, text: &str) {
        egui::Ui::label(self, text);
    }

    fn hint(&mut self, text: &str, size: f32) {
        self.label(egui::RichText::new(text).weak().size(size));
    }

    fn space(&mut self, amount: f32) {
        self.add_space(amount);
    }

    fn button(&mut self, label: &str, enabled: bool) -> bool {
        self.add_enabled(enabled, egui::Button::new(label))
            .clicked()
    }

    fn row(&mut self, body: impl FnOnce(&mut Self)) {
        self.horizontal(|ui| body(ui));
    }

    fn split_two(&mut self, body: impl FnOnce(&mut Self, &mut Self)) {
        // `columns` boxes its closure. The const form does not.
        self.columns_const(|[left, right]| body(left, right));
    }

    fn text_field(&mut self, text: &mut String, hint: &str, width: f32) {
        self.add(
            egui::TextEdit::singleline(text)
                .desired_width(width)
                .hint_text(hint),
        );
    }

    fn param_slider<P: Param>(&mut self, label: &str, param: &P, setter: &ParamSetter<'_>) {
        self.label(label);
        let gap = self.spacing().item_spacing.x;
        let value_width = readout_width(self);
        let slider_width = (self.available_width() - value_width - gap).max(1.0);
        self.horizontal(|ui| {
            ui.add(
                widgets::ParamSlider::for_param(param, setter)
                    .without_value()
                    .with_width(slider_width),
            );
            ui.add_sized(
                egui::vec2(value_width, ui.spacing().interact_size.y),
                egui::Label::new(param.to_string()).halign(egui::Align::Max),
            );
        });
    }

    fn progress(&mut self, fraction: f32, text: &str) {
        self.add(egui::ProgressBar::new(fraction).text(text));
    }

    fn canvas<R>(
        &mut self,
        breadth: Breadth,
        height: f32,
        sense: Sense,
        draw: impl FnOnce(Rect, &mut Self::Painter<'_>, Option<Point>) -> R,
    ) -> R {
        let width = match breadth {
            Breadth::Fixed(width) => width,
            Breadth::Fill => self.available_width(),
        };
        let egui_sense = match sense {
            Sense::Hover => egui::Sense::hover(),
            Sense::Click => egui::Sense::click(),
        };
        let (egui_rect, response) = self.allocate_exact_size(egui::vec2(width, height), egui_sense);
        let click = response.clicked().then(|| {
            response
                .interact_pointer_pos()
                .map(|pos| Point { x: pos.x, y: pos.y })
        });
        let mut painter = self.painter_at(egui_rect);
        draw(rect_from_egui(egui_rect), &mut painter, click.flatten())
    }

    fn request_repaint(&mut self) {
        self.ctx().request_repaint();
    }
}

impl Painter for egui::Painter {
    #[inline(always)]
    fn fill(&mut self, rect: Rect, radius: f32, color: Color) {
        self.rect_filled(rect_to_egui(rect), radius, color_to_egui(color));
    }

    #[inline(always)]
    fn line(&mut self, from: Point, to: Point, width: f32, color: Color) {
        self.line_segment(
            [egui::pos2(from.x, from.y), egui::pos2(to.x, to.y)],
            egui::Stroke::new(width, color_to_egui(color)),
        );
    }

    #[inline(always)]
    fn text_centered(&mut self, at: Point, text: &str, size: f32, color: Color) {
        egui::Painter::text(
            self,
            egui::pos2(at.x, at.y),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(size),
            color_to_egui(color),
        );
    }
}

#[inline(always)]
fn color_to_egui(color: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
}

#[inline(always)]
fn rect_to_egui(rect: Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top()),
        egui::pos2(rect.left() + rect.width(), rect.top() + rect.height()),
    )
}

#[inline(always)]
fn rect_from_egui(rect: egui::Rect) -> Rect {
    Rect::from_min_max(
        Point {
            x: rect.min.x,
            y: rect.min.y,
        },
        Point {
            x: rect.max.x,
            y: rect.max.y,
        },
    )
}

#[cfg(test)]
mod layout {
    use std::sync::Arc;

    use nice_plug::params::InternalParamMut;
    use nice_plug::prelude::{GuiContextInner, ParamPtr, ParamSetter, PluginApi, PluginState};

    use crate::editor::SonomoriEditor;
    use crate::params::SonomoriParams;
    use crate::session::{Shared, Snapshot};

    struct DummyGui;

    impl GuiContextInner for DummyGui {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Standalone
        }
        unsafe fn raw_begin_set_parameter(&self, _: ParamPtr) {}
        unsafe fn raw_set_parameter_normalized(&self, _: ParamPtr, _: f32) {}
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

    struct Frame {
        content: egui::Rect,
        tracks: Vec<egui::Rect>,
        pills: usize,
    }

    fn frame(
        width: f32,
        height: f32,
        editor: &mut SonomoriEditor,
        setter: &ParamSetter<'_>,
    ) -> Frame {
        let ctx = egui::Context::default();
        super::install_skin(&ctx);
        let mut content = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                ..Default::default()
            },
            |ui| {
                editor.show(ui, setter);
                content = ui.min_rect();
            },
        );

        let mut tracks: Vec<egui::Rect> = Vec::new();
        let mut pills = 0;
        for clipped in &output.shapes {
            let egui::epaint::Shape::Rect(shape) = &clipped.shape else {
                continue;
            };
            let rect = shape.rect;
            let radius = shape.corner_radius.nw.max(shape.corner_radius.ne);
            if radius == 0 && rect.height() > 8.0 && rect.height() < 18.0 && rect.width() > 80.0 {
                if let Some(existing) = tracks.iter_mut().find(|track| {
                    (track.top() - rect.top()).abs() < 0.5
                        && (track.left() - rect.left()).abs() < 0.5
                }) {
                    if rect.width() > existing.width() {
                        *existing = rect;
                    }
                } else {
                    tracks.push(rect);
                }
            }
        }
        tracks.sort_by(|a, b| {
            a.top()
                .total_cmp(&b.top())
                .then(a.left().total_cmp(&b.left()))
        });
        for clipped in &output.shapes {
            let egui::epaint::Shape::Rect(shape) = &clipped.shape else {
                continue;
            };
            let radius = shape.corner_radius.nw.max(shape.corner_radius.ne);
            if radius < 4 || shape.rect.height() > 24.0 {
                continue;
            }
            let center = shape.rect.center();
            if tracks.iter().any(|track| {
                center.y >= track.top() - 6.0
                    && center.y <= track.bottom() + 6.0
                    && center.x >= track.left()
                    && center.x <= track.right()
            }) {
                pills += 1;
            }
        }
        output.textures_delta.clear();
        Frame {
            content,
            tracks,
            pills,
        }
    }

    fn editor() -> (SonomoriEditor, DummyGui, Arc<SonomoriParams>) {
        let params = Arc::new(SonomoriParams::default());
        let shared = Shared::new();
        let (preview_tx, _rx) = rtrb::RingBuffer::new(8);
        let (_snap_in, snapshot) = triple_buffer::triple_buffer(&Snapshot::default());
        let editor = SonomoriEditor::new(params.clone(), shared, preview_tx, snapshot);
        (editor, DummyGui, params)
    }

    #[test]
    fn editor_fits_and_sliders_do_not_move_each_other() {
        let (mut editor, gui, params) = editor();
        let setter = ParamSetter::new(&gui);

        for size in [crate::WINDOW_SIZE, crate::MIN_WINDOW_SIZE] {
            let frame = frame(size.width, size.height, &mut editor, &setter);
            assert!(
                frame.content.width() <= size.width + 0.5,
                "content is {:.1} wide in a {} window",
                frame.content.width(),
                size.width
            );
            assert!(
                frame.content.height() <= size.height + 0.5,
                "content is {:.1} tall in a {} window",
                frame.content.height(),
                size.height
            );
            assert_eq!(frame.pills, 0, "value readouts still draw over the tracks");
            assert!(
                frame.tracks.len() >= 13,
                "expected one track per parameter, found {}",
                frame.tracks.len()
            );
        }

        let baseline = frame(
            crate::WINDOW_SIZE.width,
            crate::WINDOW_SIZE.height,
            &mut editor,
            &setter,
        );
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
            let next = frame(
                crate::WINDOW_SIZE.width,
                crate::WINDOW_SIZE.height,
                &mut editor,
                &setter,
            );
            assert_eq!(
                next.tracks, baseline.tracks,
                "moving density={density} sound={sound} sync={sync} moved other tracks"
            );
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
                floats,
                "moving one parameter wrote another"
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
    }
}

#[cfg(test)]
mod seam {
    #[test]
    fn product_code_does_not_name_the_ui_library() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        visit(&src, &mut hits);
        assert!(
            hits.is_empty(),
            "only src/ui/backend.rs may name the UI library, found {hits:?}"
        );
    }

    fn visit(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) == Some("backend.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("egui") {
                hits.push(path.display().to_string());
            }
        }
    }
}
