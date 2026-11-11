use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

use nice_plug::context::gui::GuiContext;
use nice_plug::prelude::*;

use crate::features::SliceInfo;
use crate::params::{SEED_MAX, SEED_MIN, SonomoriParams};
use crate::sample::{self, IMPORT_EXTENSIONS, SampleData};
use crate::score::{Lane, Pattern};
use crate::session::{LoadStatus, Shared, Snapshot};
use crate::slice::sensitivity_bucket;
use crate::ui::{Breadth, Color, Painter, Point, Rect, Sense, Surface};

fn lane_color(lane: Lane) -> Color {
    match lane {
        Lane::Low => Color::rgb(232, 168, 74),
        Lane::Mid => Color::rgb(224, 92, 78),
        Lane::High => Color::rgb(86, 196, 184),
        Lane::Extra => Color::rgb(164, 124, 214),
    }
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Low => "Low",
        Lane::Mid => "Mid",
        Lane::High => "High",
        Lane::Extra => "Extra",
    }
}

struct LoadFinished {
    id: u64,
    result: Result<SampleData, String>,
}

pub struct SonomoriEditor {
    gui: Option<GuiContext>,
    params: Arc<SonomoriParams>,
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
    /// `SONOMORI_LOAD` / `SONOMORI_PLAY` / `SONOMORI_READY`, used by the
    /// profiling script. Idle unless those variables are set.
    profile_checked: bool,
    profile_load_id: u64,
    profile_play: bool,
    profile_ready: Option<String>,
}

impl SonomoriEditor {
    pub(crate) fn new(
        params: Arc<SonomoriParams>,
        shared: Shared,
        preview_tx: rtrb::Producer<u32>,
        snapshot: triple_buffer::Output<Snapshot>,
    ) -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        let (import_tx, import_rx) = mpsc::channel();
        let path_input = params.sample_path.lock().clone();
        Self {
            gui: None,
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
            profile_checked: false,
            profile_load_id: 0,
            profile_play: false,
            profile_ready: None,
        }
    }

    pub(crate) fn attach_gui(&mut self, gui: GuiContext) {
        self.gui = Some(gui);
    }

    pub(crate) fn detach_gui(&mut self) {
        self.gui = None;
    }

    pub(crate) fn gui(&self) -> Option<GuiContext> {
        self.gui.clone()
    }

    pub(crate) fn params(&self) -> &Arc<SonomoriParams> {
        &self.params
    }

    fn poll_loads(&mut self) {
        while let Ok(finished) = self.load_rx.try_recv() {
            let profile = finished.id == self.profile_load_id;
            if profile {
                self.profile_load_id = 0;
            }
            if finished.id != self.active_load_id {
                if profile {
                    self.write_profile_ready("fail\n");
                }
                continue;
            }
            match finished.result {
                Ok(data) => {
                    *self.params.sample_path.lock() = data.path.clone();
                    self.path_input = data.path.clone();
                    self.shared.store_sample(data);
                    if profile && self.profile_play {
                        self.shared.audition.store(true, Ordering::Relaxed);
                    }
                    if profile {
                        self.write_profile_ready("ok\n");
                    }
                }
                Err(message) => {
                    self.shared.set_status(LoadStatus::Failed { message });
                    if profile {
                        self.write_profile_ready("fail\n");
                    }
                }
            }
        }
    }

    fn boot_profile(&mut self) {
        if self.profile_checked {
            return;
        }
        self.profile_checked = true;
        let Ok(path) = std::env::var("SONOMORI_LOAD") else {
            return;
        };
        if path.is_empty() {
            return;
        }
        self.profile_play = std::env::var_os("SONOMORI_PLAY").is_some();
        self.profile_ready = std::env::var("SONOMORI_READY")
            .ok()
            .filter(|path| !path.is_empty());
        self.begin_load(PathBuf::from(path));
        self.profile_load_id = self.active_load_id;
    }

    fn write_profile_ready(&mut self, body: &str) {
        self.profile_play = false;
        if let Some(path) = self.profile_ready.take() {
            let _ = std::fs::write(path, body);
        }
    }

    fn begin_load(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".to_string());
        self.shared.set_status(LoadStatus::Loading { name });
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

    pub(crate) fn prepare(&mut self) {
        self.boot_profile();
        self.poll_loads();
        self.poll_import();
    }

    pub(crate) fn path_input(&self) -> &str {
        &self.path_input
    }

    /// Inputs of the waveform, transport, and sliders. The playhead is not one
    /// of them: it moves every step and is drawn on its own.
    pub(crate) fn chrome_key(&mut self) -> u64 {
        let mut hash = mix(0xcbf29ce484222325, 1);
        hash = mix(hash, self.shared.standalone.load(Ordering::Relaxed) as u64);
        hash = mix(hash, self.shared.audition.load(Ordering::Relaxed) as u64);
        hash = mix(hash, self.import_open as u64);
        hash = mix(hash, self.undo_seed.is_some() as u64);
        hash = mix_str(hash, &self.path_input);
        hash = mix_float(hash, &self.params.density);
        hash = mix_float(hash, &self.params.syncopation);
        hash = mix_float(hash, &self.params.symmetry);
        hash = mix_float(hash, &self.params.pattern_var);
        hash = mix_float(hash, &self.params.sound_var);
        hash = mix_float(hash, &self.params.swing);
        hash = mix_float(hash, &self.params.decay);
        hash = mix_float(hash, &self.params.tone);
        hash = mix_float(hash, &self.params.sensitivity);
        hash = mix_float(hash, &self.params.tempo);
        hash = mix_float(hash, &self.params.output);
        hash = mix_int(hash, &self.params.pitch);
        hash = mix_int(hash, &self.params.bars);
        hash = mix_int(hash, &self.params.seed);
        let loaded = self.shared.sample.load();
        hash = mix(
            hash,
            loaded
                .as_ref()
                .map(|data| Arc::as_ptr(data) as u64)
                .unwrap_or(0),
        );
        let snapshot = self.snapshot.read();
        for lane in snapshot.pattern.lanes {
            hash = mix(hash, lane.start as u64);
            hash = mix(hash, lane.end as u64);
        }
        hash = mix(hash, snapshot.clock.host_playing as u64);
        hash = mix(hash, (snapshot.clock.bpm * 10.0).round() as i16 as u64);
        let status = self.shared.status.lock();
        mix_status(hash, &status)
    }

    pub(crate) fn wants_pump(&self) -> bool {
        self.import_open || matches!(*self.shared.status.lock(), LoadStatus::Loading { .. })
    }

    /// Egui and rune paint the whole screen through this. The iced editor
    /// paints the pieces below so a playhead step can skip the waveform.
    #[cfg_attr(not(any(test, ui_egui, ui_rune)), allow(dead_code))]
    pub(crate) fn show(&mut self, ui: &mut impl Surface, setter: &ParamSetter<'_>) {
        self.prepare();
        self.paint_top(ui, setter);
        self.paint_grid(ui);
        self.paint_sliders(ui, setter);
        self.paint_readout(ui);
        if self.wants_pump() {
            ui.request_repaint();
        }
    }

    pub(crate) fn paint_top(&mut self, ui: &mut impl Surface, setter: &ParamSetter<'_>) {
        let sample = self.shared.sample.load_full();
        let snapshot = *self.snapshot.read();
        let mut queued_load = None;
        let mut request_import = false;
        let bucket = sensitivity_bucket(self.params.sensitivity.value());
        let slices = sample
            .as_ref()
            .and_then(|loaded| loaded.buckets.get(bucket))
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        ui.heading(crate::Sonomori::NAME);
        ui.label("Import a sound, chop it on transients, and play a generated loop.");
        ui.space(4.0);

        ui.row(|ui| {
            if ui.button("Import", !self.import_open) {
                request_import = true;
            }
            if ui.button("Randomize", true) {
                randomize(setter, &self.params, &mut self.undo_seed);
            }
            if ui.button("Undo", self.undo_seed.is_some())
                && let Some(seed) = self.undo_seed.take()
            {
                set_int(setter, &self.params.seed, seed);
            }
            ui.label(&format!("Seed {}", self.params.seed.value()));
        });

        ui.row(|ui| {
            let standalone = self.shared.standalone.load(Ordering::Relaxed);
            let audition = self.shared.audition.load(Ordering::Relaxed);
            let label = if audition {
                "Stop"
            } else if standalone {
                "Play"
            } else {
                "Audition"
            };
            if ui.button(label, true) {
                self.shared.audition.store(!audition, Ordering::Relaxed);
            }
            if snapshot.clock.host_playing {
                ui.label(&format!("Host is playing at {:.1} BPM", snapshot.clock.bpm));
            } else if audition {
                ui.label(&format!(
                    "{} at {:.1} BPM",
                    if standalone { "Playing" } else { "Audition" },
                    snapshot.clock.bpm
                ));
            } else if standalone {
                ui.label("Press Play to start the loop.");
            } else {
                ui.label("Press play in the DAW, or Audition while it is stopped.");
            }
        });

        ui.row(|ui| {
            ui.text_field(&mut self.path_input, "or paste a file path", 360.0);
            if ui.button("Load path", true) {
                let path = PathBuf::from(self.path_input.trim());
                if !path.as_os_str().is_empty() {
                    queued_load = Some(path);
                }
            }
        });

        ui.label(&status_line(&self.shared, sample.as_deref(), slices.len()));

        if let Some(loaded) = sample.as_ref() {
            paint_waveform(ui, loaded, slices, &snapshot, &mut self.preview_tx);
        } else {
            ui.canvas(Breadth::Fill, 110.0, Sense::Hover, |rect, painter, _| {
                painter.fill(rect, 4.0, Color::rgb(22, 24, 28));
                painter.text_centered(
                    rect.center(),
                    "No sound loaded",
                    14.0,
                    Color::rgb(140, 144, 150),
                );
            });
        }

        ui.space(6.0);
        ui.label("Pattern");

        if let Some(path) = queued_load {
            self.begin_load(path);
        }
        if request_import {
            self.import_open = true;
            ui.request_repaint();
            open_import_dialog(self.import_tx.clone());
            // Outside macOS the panel runs before this returns. On macOS it is
            // queued, and a later frame reads the chosen path.
            self.poll_import();
        }
    }

    pub(crate) fn paint_grid(&mut self, ui: &mut impl Surface) {
        let snapshot = *self.snapshot.read();
        paint_pattern(ui, &snapshot);
    }

    pub(crate) fn paint_sliders(&mut self, ui: &mut impl Surface, setter: &ParamSetter<'_>) {
        ui.space(8.0);
        let params = &self.params;
        ui.split_two(|left, right| {
            left.param_slider("Density", &params.density, setter);
            left.param_slider("Syncopation", &params.syncopation, setter);
            left.param_slider("Symmetry", &params.symmetry, setter);
            left.param_slider("Pattern variation", &params.pattern_var, setter);
            left.param_slider("Swing", &params.swing, setter);
            left.param_slider("Bars", &params.bars, setter);

            right.param_slider("Sound variation", &params.sound_var, setter);
            right.param_slider("Decay", &params.decay, setter);
            right.param_slider("Pitch", &params.pitch, setter);
            right.param_slider("Tone", &params.tone, setter);
            right.param_slider("Slice sensitivity", &params.sensitivity, setter);
            let tempo_label = if self.shared.standalone.load(Ordering::Relaxed) {
                "Tempo"
            } else {
                "Audition tempo"
            };
            right.param_slider(tempo_label, &params.tempo, setter);
            right.param_slider("Output", &params.output, setter);
        });
    }

    pub(crate) fn paint_readout(&mut self, ui: &mut impl Surface) {
        ui.space(6.0);
        let meter = self.shared.meter.load(Ordering::Relaxed);
        let db = if meter > 1.0e-8 {
            20.0 * meter.log10()
        } else {
            -80.0
        };
        let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        ui.progress(normalized, &format!("{db:.1} dB"));
        ui.hint(
            "Click a slice to preview it. Randomize picks new slices and a new groove. The macros reshape that groove.",
            12.0,
        );
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
            .add_filter("Audio", IMPORT_EXTENSIONS)
            .pick_file();
        let _ = tx.send(path);
    };

    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(show);
    #[cfg(not(target_os = "macos"))]
    show();
}

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x100000001b3)
}

fn mix_f32(hash: u64, value: f32) -> u64 {
    mix(hash, value.to_bits() as u64)
}

fn mix_str(mut hash: u64, text: &str) -> u64 {
    for byte in text.bytes() {
        hash = mix(hash, byte as u64);
    }
    hash
}

fn mix_float(hash: u64, param: &nice_plug::prelude::FloatParam) -> u64 {
    let hash = mix_f32(hash, param.value());
    mix_f32(hash, param.modulated_normalized_value())
}

fn mix_int(hash: u64, param: &nice_plug::prelude::IntParam) -> u64 {
    let hash = mix(hash, param.value() as u32 as u64);
    mix_f32(hash, param.modulated_normalized_value())
}

fn mix_status(hash: u64, status: &LoadStatus) -> u64 {
    match status {
        LoadStatus::Idle => mix(hash, 1),
        LoadStatus::Loading { name } => mix_str(mix(hash, 2), name),
        LoadStatus::Failed { message } => mix_str(mix(hash, 3), message),
    }
}

fn randomize(setter: &ParamSetter<'_>, params: &SonomoriParams, undo_seed: &mut Option<i32>) {
    let current = params.seed.value();
    *undo_seed = Some(current);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(1);
    let span = (SEED_MAX - SEED_MIN + 1) as u128;
    let mut next = (nanos % span) as i32 + SEED_MIN;
    if next == current {
        next = if current >= SEED_MAX {
            SEED_MIN
        } else {
            current + 1
        };
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
                let trimmed = sample::trimmed_note(loaded.trimmed);
                format!(
                    "{}  ·  {} slices  ·  {:.1}s{trimmed}",
                    loaded.name, slice_count, loaded.seconds
                )
            }
            None => sample::idle_hint(),
        },
    }
}

fn paint_waveform(
    ui: &mut impl Surface,
    sample: &SampleData,
    slices: &[SliceInfo],
    snapshot: &Snapshot,
    preview_tx: &mut rtrb::Producer<u32>,
) {
    ui.canvas(
        Breadth::Fill,
        128.0,
        Sense::Click,
        |rect, painter, click| {
            painter.fill(rect, 4.0, Color::rgb(22, 24, 28));
            if sample.len() > 0 && rect.width() >= 2.0 {
                paint_waveform_contents(painter, rect, sample, slices, snapshot, click, preview_tx);
            }
        },
    );
}

fn paint_waveform_contents(
    painter: &mut impl Painter,
    rect: Rect,
    sample: &SampleData,
    slices: &[SliceInfo],
    snapshot: &Snapshot,
    click: Option<Point>,
    preview_tx: &mut rtrb::Producer<u32>,
) {
    let frames = sample.len() as f32;
    for lane in Lane::ALL {
        let played = &snapshot.pattern.lanes[lane.index()];
        if played.end <= played.start {
            continue;
        }
        let start_x = rect.left() + rect.width() * played.start as f32 / frames;
        let end_x = rect.left() + rect.width() * played.end as f32 / frames;
        let region = Rect::from_min_max(
            Point {
                x: start_x,
                y: rect.top(),
            },
            Point {
                x: end_x.max(start_x + 1.0),
                y: rect.bottom(),
            },
        );
        painter.fill(region, 0.0, lane_color(lane).fade(0.28));
    }

    let mid_y = rect.center().y;
    let scale = rect.height() * 0.46 / sample.display_peak;
    let columns = sample.peaks.len().max(1);
    let stroke = Color::rgb(210, 214, 220);
    for (index, peak) in sample.peaks.iter().enumerate() {
        let x = rect.left() + rect.width() * index as f32 / columns as f32;
        painter.line(
            Point {
                x,
                y: mid_y - peak.max * scale,
            },
            Point {
                x,
                y: mid_y - peak.min * scale,
            },
            1.0,
            stroke,
        );
    }

    let mark = Color::rgba(255, 255, 255, 40);
    for slice in slices {
        let x = rect.left() + rect.width() * slice.start as f32 / frames;
        painter.line(
            Point { x, y: rect.top() },
            Point {
                x,
                y: rect.bottom(),
            },
            1.0,
            mark,
        );
    }

    if let Some(pointer) = click {
        let position = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 0.999);
        let frame = (position * frames) as usize;
        if let Some(index) = slices
            .iter()
            .position(|slice| frame >= slice.start && frame < slice.end)
        {
            let _ = preview_tx.push(index as u32);
        }
    }
}

fn paint_pattern(ui: &mut impl Surface, snapshot: &Snapshot) {
    let pattern: &Pattern = &snapshot.pattern;
    let steps = pattern.steps.max(1);
    for lane in Lane::ALL {
        ui.row(|ui| {
            let color = lane_color(lane);
            ui.canvas(
                Breadth::Fixed(10.0),
                10.0,
                Sense::Hover,
                |swatch, painter, _| {
                    painter.fill(swatch, 2.0, color);
                },
            );
            ui.label(lane_name(lane));
        });
        ui.canvas(Breadth::Fill, 18.0, Sense::Hover, |rect, painter, _| {
            let cell = rect.width() / steps as f32;
            for step in 0..steps {
                let x = rect.left() + cell * step as f32;
                let cell_rect = Rect::from_min_size(
                    Point {
                        x: x + 1.0,
                        y: rect.top(),
                    },
                    (cell - 2.0).max(1.0),
                    rect.height(),
                );
                let velocity = pattern.lanes[lane.index()].velocity[step];
                let color = if velocity > 0.0 {
                    lane_color(lane).fade(0.35 + 0.65 * velocity)
                } else {
                    Color::rgb(38, 40, 46)
                };
                painter.fill(cell_rect, 2.0, color);
                if snapshot.playhead == Some(step) {
                    painter.highlight(cell_rect.shrink(1.0), 1.0, Color::rgba(255, 255, 255, 70));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nice_plug::params::InternalParamMut;

    use crate::params::SonomoriParams;
    use crate::session::{Shared, Snapshot};

    use super::SonomoriEditor;

    #[test]
    fn chrome_key_ignores_the_playhead() {
        let params = Arc::new(SonomoriParams::default());
        let (mut input, output) = triple_buffer::triple_buffer(&Snapshot::default());
        let (preview_tx, _rx) = rtrb::RingBuffer::new(4);
        let mut editor = SonomoriEditor::new(params, Shared::new(), preview_tx, output);
        let first = editor.chrome_key();

        let mut moved = Snapshot::default();
        moved.playhead = Some(7);
        input.write(moved);
        assert_eq!(editor.chrome_key(), first);

        let mut retimed = Snapshot::default();
        retimed.clock.bpm = 99.0;
        input.write(retimed);
        assert_ne!(editor.chrome_key(), first);
    }

    #[test]
    fn chrome_key_tracks_a_slider() {
        let params = Arc::new(SonomoriParams::default());
        let (_input, output) = triple_buffer::triple_buffer(&Snapshot::default());
        let (preview_tx, _rx) = rtrb::RingBuffer::new(4);
        let mut editor = SonomoriEditor::new(params, Shared::new(), preview_tx, output);
        let first = editor.chrome_key();
        unsafe {
            editor
                .params
                .tempo
                ._internal_set_plain_value(editor.params.tempo.value() + 10.0);
        }
        assert_ne!(editor.chrome_key(), first);
    }
}
