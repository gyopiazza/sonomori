//! Basic controls: import, waveform, pattern, and the loop macros.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};
use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::*;
use nice_plug_egui::{NiceEguiApp, widgets};

use crate::pattern::{Pattern, LANES};
use crate::sample::SampleData;
use crate::slice::sensitivity_bucket;
use crate::{LoadStatus, Shared, Snapshot};

const LANE_COLORS: [Color32; LANES] = [
    Color32::from_rgb(232, 168, 74),
    Color32::from_rgb(224, 92, 78),
    Color32::from_rgb(86, 196, 184),
    Color32::from_rgb(164, 124, 214),
];
const LANE_NAMES: [&str; LANES] = ["Low", "Mid", "High", "Extra"];

struct OpenEditorState {
    nice_gui_ctx: GuiContext,
}

struct LoadFinished {
    id: u64,
    result: Result<SampleData, String>,
}

pub struct SonomoriEditor {
    open_state: Option<OpenEditorState>,
    params: Arc<crate::SonomoriParams>,
    shared: Shared,
    preview_tx: rtrb::Producer<u32>,
    snapshot: triple_buffer::Output<Snapshot>,
    path_input: String,
    undo_seed: Option<i32>,
    load_tx: Sender<LoadFinished>,
    load_rx: Receiver<LoadFinished>,
    next_load_id: u64,
    active_load_id: u64,
    import_tx: Sender<Option<PathBuf>>,
    import_rx: Receiver<Option<PathBuf>>,
    import_open: bool,
}

impl SonomoriEditor {
    pub(crate) fn new(
        params: Arc<crate::SonomoriParams>,
        shared: Shared,
        preview_tx: rtrb::Producer<u32>,
        snapshot: triple_buffer::Output<Snapshot>,
    ) -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        let (import_tx, import_rx) = mpsc::channel();
        let path_input = params.sample_path.lock().clone();
        Self {
            open_state: None,
            params,
            shared,
            preview_tx,
            snapshot,
            path_input,
            undo_seed: None,
            load_tx,
            load_rx,
            next_load_id: 1,
            active_load_id: 0,
            import_tx,
            import_rx,
            import_open: false,
        }
    }

    fn poll_loads(&mut self) {
        while let Ok(finished) = self.load_rx.try_recv() {
            if finished.id != self.active_load_id {
                continue;
            }
            match finished.result {
                Ok(data) => {
                    *self.params.sample_path.lock() = data.path.clone();
                    self.path_input = data.path.clone();
                    self.shared.sample.store(Some(Arc::new(data)));
                    *self.shared.status.lock() = LoadStatus::Idle;
                }
                Err(message) => {
                    *self.shared.status.lock() = LoadStatus::Failed { message };
                }
            }
        }
    }

    fn begin_load(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".to_string());
        *self.shared.status.lock() = LoadStatus::Loading { name };
        let id = self.next_load_id;
        self.next_load_id = self.next_load_id.wrapping_add(1);
        self.active_load_id = id;
        let tx = self.load_tx.clone();
        std::thread::spawn(move || {
            let result = crate::sample::load(&path);
            let _ = tx.send(LoadFinished { id, result });
        });
    }

    fn poll_import(&mut self) {
        while let Ok(path) = self.import_rx.try_recv() {
            self.import_open = false;
            if let Some(path) = path {
                self.begin_load(path);
            }
        }
    }
}

impl NiceEguiApp for SonomoriEditor {
    fn build(
        &mut self,
        egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        _frame: &mut nice_plug_egui::Frame,
    ) -> Result<(), nice_plug_egui::baseview::HandlerError> {
        egui_ctx.set_theme(egui::Theme::Dark);
        egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        });
        self.open_state = Some(OpenEditorState { nice_gui_ctx });
        Ok(())
    }

    fn editor_closed(&mut self) {
        self.open_state = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut nice_plug_egui::Frame) {
        self.poll_loads();
        self.poll_import();
        let sample = self.shared.sample.load_full();
        let snapshot = *self.snapshot.read();
        let mut queued_load = None;
        let mut request_import = false;
        draw(
            ui,
            &self.open_state,
            &self.params,
            &self.shared,
            &mut self.preview_tx,
            &mut self.path_input,
            &mut self.undo_seed,
            &sample,
            &snapshot,
            &mut queued_load,
            self.import_open,
            &mut request_import,
        );
        if let Some(path) = queued_load {
            self.begin_load(path);
        }
        if request_import {
            self.import_open = true;
            ui.ctx().request_repaint();
            open_import_dialog(self.import_tx.clone());
            // Outside macOS the panel runs before this returns. On macOS it is
            // queued, and a later frame reads the chosen path.
            self.poll_import();
        }
    }
}

/// Open the file panel outside the editor's display-link callback.
///
/// macOS aborts if `NSSavePanel.runModal` is called from the display-link
/// callback that paints this window.
fn open_import_dialog(tx: Sender<Option<PathBuf>>) {
    let show = move || {
        let path = rfd::FileDialog::new()
            .set_title("Import a sound")
            .add_filter("Audio", &["wav", "mp3", "flac", "ogg", "aac", "m4a"])
            .pick_file();
        let _ = tx.send(path);
    };

    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(show);
    #[cfg(not(target_os = "macos"))]
    show();
}

fn draw(
    ui: &mut egui::Ui,
    open_state: &Option<OpenEditorState>,
    params: &crate::SonomoriParams,
    shared: &Shared,
    preview_tx: &mut rtrb::Producer<u32>,
    path_input: &mut String,
    undo_seed: &mut Option<i32>,
    sample: &Option<Arc<SampleData>>,
    snapshot: &Snapshot,
    queued_load: &mut Option<PathBuf>,
    import_open: bool,
    request_import: &mut bool,
) {
    let Some(state) = open_state.as_ref() else {
        return;
    };
    let setter = state.nice_gui_ctx.param_setter();
    let bucket = sensitivity_bucket(params.sensitivity.value());
    let slices = sample
        .as_ref()
        .and_then(|loaded| loaded.buckets.get(bucket))
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    ui.heading(crate::Sonomori::NAME);
    ui.label("Import a sound, chop it on transients, and play a generated loop.");
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!import_open, egui::Button::new("Import"))
            .clicked()
        {
            *request_import = true;
        }
        if ui.button("Randomize").clicked() {
            randomize(&setter, params, undo_seed);
        }
        let undo_enabled = undo_seed.is_some();
        if ui
            .add_enabled(undo_enabled, egui::Button::new("Undo"))
            .clicked()
            && let Some(seed) = undo_seed.take()
        {
            set_int(&setter, &params.seed, seed);
        }
        ui.label(format!("Seed {}", params.seed.value()));
    });

    ui.horizontal(|ui| {
        let standalone = shared.standalone.load(Ordering::Relaxed);
        let audition = shared.audition.load(Ordering::Relaxed);
        let label = if audition {
            "Stop"
        } else if standalone {
            "Play"
        } else {
            "Audition"
        };
        if ui.button(label).clicked() {
            shared.audition.store(!audition, Ordering::Relaxed);
        }
        if snapshot.host_playing {
            ui.label(format!("Host is playing at {:.1} BPM", snapshot.bpm));
        } else if audition {
            ui.label(format!(
                "{} at {:.1} BPM",
                if standalone { "Playing" } else { "Audition" },
                snapshot.bpm
            ));
        } else if standalone {
            ui.label("Press Play to start the loop.");
        } else {
            ui.label("Press play in the DAW, or Audition while it is stopped.");
        }
    });

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(path_input)
                .desired_width(360.0)
                .hint_text("or paste a file path"),
        );
        if ui.button("Load path").clicked() {
            let path = PathBuf::from(path_input.trim());
            if !path.as_os_str().is_empty() {
                *queued_load = Some(path);
            }
        }
    });

    ui.label(status_line(
        shared,
        sample.as_deref(),
        slices.len(),
    ));

    if let Some(loaded) = sample.as_ref() {
        paint_waveform(ui, loaded, slices, snapshot, preview_tx);
    } else {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 110.0), Sense::hover());
        ui.painter().rect_filled(rect, 4.0, Color32::from_rgb(22, 24, 28));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No sound loaded",
            egui::FontId::proportional(14.0),
            Color32::from_rgb(140, 144, 150),
        );
    }

    ui.add_space(6.0);
    ui.label("Pattern");
    paint_pattern(ui, snapshot);

    ui.add_space(8.0);
    ui.columns(2, |columns| {
        add_slider(&mut columns[0], "Density", &params.density, &setter);
        add_slider(&mut columns[0], "Syncopation", &params.syncopation, &setter);
        add_slider(&mut columns[0], "Symmetry", &params.symmetry, &setter);
        add_slider(
            &mut columns[0],
            "Pattern variation",
            &params.pattern_var,
            &setter,
        );
        add_slider(&mut columns[0], "Swing", &params.swing, &setter);
        add_slider(&mut columns[0], "Bars", &params.bars, &setter);

        add_slider(
            &mut columns[1],
            "Sound variation",
            &params.sound_var,
            &setter,
        );
        add_slider(&mut columns[1], "Decay", &params.decay, &setter);
        add_slider(&mut columns[1], "Pitch", &params.pitch, &setter);
        add_slider(&mut columns[1], "Tone", &params.tone, &setter);
        add_slider(
            &mut columns[1],
            "Slice sensitivity",
            &params.sensitivity,
            &setter,
        );
        let tempo_label = if shared.standalone.load(Ordering::Relaxed) {
            "Tempo"
        } else {
            "Audition tempo"
        };
        add_slider(&mut columns[1], tempo_label, &params.tempo, &setter);
        add_slider(&mut columns[1], "Output", &params.output, &setter);
    });

    ui.add_space(6.0);
    let meter = shared.meter.load(Ordering::Relaxed);
    let db = if meter > 1.0e-8 {
        20.0 * meter.log10()
    } else {
        -80.0
    };
    let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    ui.add(egui::ProgressBar::new(normalized).text(format!("{db:.1} dB")));
    ui.label(
        egui::RichText::new(
            "Click a slice to preview it. Randomize picks new slices and a new groove. The macros reshape that groove.",
        )
        .weak()
        .size(12.0),
    );

    if snapshot.playing
        || meter > 0.001
        || import_open
        || matches!(*shared.status.lock(), LoadStatus::Loading { .. })
    {
        ui.ctx().request_repaint();
    }
}

fn add_slider<P: Param>(ui: &mut egui::Ui, label: &str, param: &P, setter: &ParamSetter<'_>) {
    ui.label(label);
    ui.add(widgets::ParamSlider::for_param(param, setter).with_width(ui.available_width()));
}

fn randomize(setter: &ParamSetter<'_>, params: &crate::SonomoriParams, undo_seed: &mut Option<i32>) {
    let current = params.seed.value();
    *undo_seed = Some(current);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(1);
    let mut next = (nanos % 999_999) as i32 + 1;
    if next == current {
        next = if current >= 999_999 { 1 } else { current + 1 };
    }
    set_int(setter, &params.seed, next);
}

fn set_int(setter: &ParamSetter<'_>, param: &IntParam, value: i32) {
    setter.begin_set_parameter(param);
    setter.set_parameter(param, value);
    setter.end_set_parameter(param);
}

fn status_line(shared: &Shared, sample: Option<&SampleData>, slice_count: usize) -> String {
    let status = shared.status.lock();
    match &*status {
        LoadStatus::Loading { name } => format!("Loading {name}..."),
        LoadStatus::Failed { message } => message.clone(),
        LoadStatus::Idle => match sample {
            Some(loaded) => {
                let trimmed = if loaded.trimmed {
                    "  ·  using the first 30 seconds"
                } else {
                    ""
                };
                format!(
                    "{}  ·  {} slices  ·  {:.1}s{trimmed}",
                    loaded.name, slice_count, loaded.seconds
                )
            }
            None => "Import a WAV, MP3, FLAC, OGG, AAC, or M4A file. Longer files use the first 30 seconds.".into(),
        },
    }
}

fn paint_waveform(
    ui: &mut egui::Ui,
    sample: &SampleData,
    slices: &[crate::sample::SliceInfo],
    snapshot: &Snapshot,
    preview_tx: &mut rtrb::Producer<u32>,
) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 128.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(22, 24, 28));
    if sample.len() == 0 || rect.width() < 2.0 {
        return;
    }

    for (lane, lane_pattern) in snapshot.pattern.lanes.iter().enumerate() {
        let Some(slice) = slices.get(lane_pattern.slice) else {
            continue;
        };
        let start_x = rect.left() + rect.width() * slice.start as f32 / sample.len() as f32;
        let end_x = rect.left() + rect.width() * slice.end as f32 / sample.len() as f32;
        let region = Rect::from_min_max(
            Pos2::new(start_x, rect.top()),
            Pos2::new(end_x.max(start_x + 1.0), rect.bottom()),
        );
        painter.rect_filled(region, 0.0, LANE_COLORS[lane].gamma_multiply(0.28));
    }

    let mid_y = rect.center().y;
    let scale = rect.height() * 0.46 / sample.display_peak;
    let columns = sample.peaks.len().max(1);
    for (index, peak) in sample.peaks.iter().enumerate() {
        let x = rect.left() + rect.width() * index as f32 / columns as f32;
        let y0 = mid_y - peak.max * scale;
        let y1 = mid_y - peak.min * scale;
        painter.line_segment(
            [Pos2::new(x, y0), Pos2::new(x, y1)],
            Stroke::new(1.0, Color32::from_rgb(210, 214, 220)),
        );
    }

    for slice in slices {
        let x = rect.left() + rect.width() * slice.start as f32 / sample.len() as f32;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
        );
    }

    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let position = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 0.999);
        let frame = (position * sample.len() as f32) as usize;
        if let Some(index) = slices
            .iter()
            .position(|slice| frame >= slice.start && frame < slice.end)
        {
            let _ = preview_tx.push(index as u32);
        }
    }
}

fn paint_pattern(ui: &mut egui::Ui, snapshot: &Snapshot) {
    let pattern: &Pattern = &snapshot.pattern;
    let steps = pattern.steps.max(1);
    for lane in 0..LANES {
        ui.horizontal(|ui| {
            let (swatch, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(swatch, 2.0, LANE_COLORS[lane]);
            ui.label(LANE_NAMES[lane]);
        });
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
        let cell = rect.width() / steps as f32;
        let painter = ui.painter();
        for step in 0..steps {
            let x = rect.left() + cell * step as f32;
            let cell_rect = Rect::from_min_size(
                Pos2::new(x + 1.0, rect.top()),
                Vec2::new((cell - 2.0).max(1.0), rect.height()),
            );
            let velocity = pattern.lanes[lane].velocity[step];
            let color = if velocity > 0.0 {
                LANE_COLORS[lane].gamma_multiply(0.35 + 0.65 * velocity)
            } else {
                Color32::from_rgb(38, 40, 46)
            };
            painter.rect_filled(cell_rect, 2.0, color);
            if snapshot.playhead == Some(step) {
                painter.rect_filled(
                    cell_rect.shrink(1.0),
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 70),
                );
            }
        }
    }
}
