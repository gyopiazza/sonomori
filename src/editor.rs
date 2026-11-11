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
use crate::session::{LoadStatus, METER_ACTIVE, Shared, Snapshot};
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
            if finished.id != self.active_load_id {
                continue;
            }
            match finished.result {
                Ok(data) => {
                    *self.params.sample_path.lock() = data.path.clone();
                    self.path_input = data.path.clone();
                    self.shared.store_sample(data);
                }
                Err(message) => {
                    self.shared.set_status(LoadStatus::Failed { message });
                }
            }
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

    pub(crate) fn show(&mut self, ui: &mut impl Surface, setter: &ParamSetter<'_>) {
        self.poll_loads();
        self.poll_import();
        let sample = self.shared.sample.load_full();
        let snapshot = *self.snapshot.read();
        let mut queued_load = None;
        let mut request_import = false;
        draw(
            ui,
            setter,
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
            ui.request_repaint();
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
            .add_filter("Audio", IMPORT_EXTENSIONS)
            .pick_file();
        let _ = tx.send(path);
    };

    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(show);
    #[cfg(not(target_os = "macos"))]
    show();
}

fn draw(
    ui: &mut impl Surface,
    setter: &ParamSetter<'_>,
    params: &SonomoriParams,
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
    let bucket = sensitivity_bucket(params.sensitivity.value());
    let slices = sample
        .as_ref()
        .and_then(|loaded| loaded.buckets.get(bucket))
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    ui.heading(crate::Sonomori::NAME);
    ui.label("Import a sound, chop it on transients, and play a generated loop.");
    ui.space(4.0);

    ui.row(|ui| {
        if ui.button("Import", !import_open) {
            *request_import = true;
        }
        if ui.button("Randomize", true) {
            randomize(setter, params, undo_seed);
        }
        if ui.button("Undo", undo_seed.is_some())
            && let Some(seed) = undo_seed.take()
        {
            set_int(setter, &params.seed, seed);
        }
        ui.label(&format!("Seed {}", params.seed.value()));
    });

    ui.row(|ui| {
        let standalone = shared.standalone.load(Ordering::Relaxed);
        let audition = shared.audition.load(Ordering::Relaxed);
        let label = if audition {
            "Stop"
        } else if standalone {
            "Play"
        } else {
            "Audition"
        };
        if ui.button(label, true) {
            shared.audition.store(!audition, Ordering::Relaxed);
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
        ui.text_field(path_input, "or paste a file path", 360.0);
        if ui.button("Load path", true) {
            let path = PathBuf::from(path_input.trim());
            if !path.as_os_str().is_empty() {
                *queued_load = Some(path);
            }
        }
    });

    ui.label(&status_line(shared, sample.as_deref(), slices.len()));

    if let Some(loaded) = sample.as_ref() {
        paint_waveform(ui, loaded, slices, snapshot, preview_tx);
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
    paint_pattern(ui, snapshot);

    ui.space(8.0);
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
        let tempo_label = if shared.standalone.load(Ordering::Relaxed) {
            "Tempo"
        } else {
            "Audition tempo"
        };
        right.param_slider(tempo_label, &params.tempo, setter);
        right.param_slider("Output", &params.output, setter);
    });

    ui.space(6.0);
    let meter = shared.meter.load(Ordering::Relaxed);
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

    if snapshot.playing
        || meter > METER_ACTIVE
        || import_open
        || matches!(*shared.status.lock(), LoadStatus::Loading { .. })
    {
        ui.request_repaint();
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
                    painter.fill(cell_rect.shrink(1.0), 1.0, Color::rgba(255, 255, 255, 70));
                }
            }
        });
    }
}
