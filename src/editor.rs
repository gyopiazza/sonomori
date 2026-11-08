//! Editor controls: import, waveform, pattern, and the loop macros.
//!
//! The widget tree is iced. Meters and the playhead still arrive through the
//! audio thread's `PollSubNotifier`; iced only delivers `Message::Poll` when
//! that flag is set. File-dialog and decode waits use their own timer because
//! nothing on the audio thread is ticking then.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use nice_plug::prelude::*;
use nice_plug_iced::iced::widget::canvas::{self, Frame, Stroke};
use nice_plug_iced::iced::widget::{
    Space, button, column, container, progress_bar, row, scrollable, slider, text, text_input,
};
use nice_plug_iced::iced::{
    self, Center, Color, Element, Length, Point, Rectangle, Size, Subscription, Theme, mouse,
};
use nice_plug_iced::{IcedNiceContext, PersistentState};

use crate::pattern::{LANES, MAX_STEPS, Pattern};
use crate::sample::SampleData;
use crate::slice::sensitivity_bucket;
use crate::{LoadStatus, Shared, Snapshot};

const LANE_RGB: [[u8; 3]; LANES] = [
    [232, 168, 74],
    [224, 92, 78],
    [86, 196, 184],
    [164, 124, 214],
];
const LANE_NAMES: [&str; LANES] = ["Low", "Mid", "High", "Extra"];

const LEFT_SLIDERS: [SliderId; 6] = [
    SliderId::Density,
    SliderId::Syncopation,
    SliderId::Symmetry,
    SliderId::PatternVar,
    SliderId::Swing,
    SliderId::Bars,
];
const RIGHT_SLIDERS: [SliderId; 7] = [
    SliderId::SoundVar,
    SliderId::Decay,
    SliderId::Pitch,
    SliderId::Tone,
    SliderId::Sensitivity,
    SliderId::Tempo,
    SliderId::Output,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliderId {
    Density,
    Syncopation,
    Symmetry,
    PatternVar,
    Swing,
    Bars,
    SoundVar,
    Decay,
    Pitch,
    Tone,
    Sensitivity,
    Tempo,
    Output,
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Poll,
    WindowResized,
    Import,
    Randomize,
    Undo,
    ToggleAudition,
    PathChanged(String),
    LoadPath,
    SliderChanged(SliderId, f32),
    SliderReleased(SliderId),
    /// Horizontal position inside the waveform, `0.0..1.0`.
    Preview(f32),
}

struct LoadFinished {
    id: u64,
    result: Result<SampleData, String>,
}

pub(crate) struct EditorModel {
    params: Arc<crate::SonomoriParams>,
    shared: Shared,
    preview_tx: rtrb::Producer<u32>,
    /// `read` needs `&mut`, and iced's `view` only has `&self`.
    snapshot_rx: triple_buffer::Output<Snapshot>,
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

impl EditorModel {
    pub(crate) fn new(
        params: Arc<crate::SonomoriParams>,
        shared: Shared,
        preview_tx: rtrb::Producer<u32>,
        snapshot_rx: triple_buffer::Output<Snapshot>,
    ) -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        let (import_tx, import_rx) = mpsc::channel();
        let path_input = params.sample_path.lock().clone();
        Self {
            params,
            shared,
            preview_tx,
            snapshot_rx,
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
}

pub(crate) struct Gui {
    model: PersistentState<EditorModel>,
    ctx: IcedNiceContext,
    dragging: Option<SliderId>,
    snapshot: Snapshot,
    sample: Option<Arc<SampleData>>,
}

impl Gui {
    pub(crate) fn new(model: PersistentState<EditorModel>, ctx: IcedNiceContext) -> Self {
        let mut gui = Self {
            model,
            ctx,
            dragging: None,
            snapshot: Snapshot::default(),
            sample: None,
        };
        gui.refresh();
        gui
    }

    pub(crate) fn theme(&self) -> Option<Theme> {
        Some(Theme::Dark)
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.ctx.user_scale_factor()
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::poll_events().map(|_| Message::Poll),
            iced::window_resized().map(|_| Message::WindowResized),
        ];
        // `iced::time::every` is not wired through the thread-pool backend.
        if self.waiting_on_disk() {
            subscriptions.push(Subscription::run(disk_wait));
        }
        Subscription::batch(subscriptions)
    }

    pub(crate) fn update(&mut self, message: Message) {
        match message {
            Message::Poll => self.refresh(),
            Message::WindowResized => self.ctx.sync_window_size(),
            Message::Import => {
                self.model.import_open = true;
                open_import_dialog(self.model.import_tx.clone());
                // Outside macOS the panel runs before this returns. On macOS it
                // is queued, and a later poll reads the chosen path.
                self.refresh();
            }
            Message::Randomize => {
                let params = Arc::clone(&self.model.params);
                let setter = self.ctx.nice_context.param_setter();
                randomize(&setter, &params, &mut self.model.undo_seed);
            }
            Message::Undo => {
                if let Some(seed) = self.model.undo_seed.take() {
                    let setter = self.ctx.nice_context.param_setter();
                    set_int(&setter, &self.model.params.seed, seed);
                }
            }
            Message::ToggleAudition => {
                let audition = self.model.shared.audition.load(Ordering::Relaxed);
                self.model
                    .shared
                    .audition
                    .store(!audition, Ordering::Relaxed);
            }
            Message::PathChanged(path) => self.model.path_input = path,
            Message::LoadPath => {
                let path = PathBuf::from(self.model.path_input.trim());
                if !path.as_os_str().is_empty() {
                    self.begin_load(path);
                }
            }
            Message::SliderChanged(id, value) => {
                let begin = self.dragging != Some(id);
                if begin {
                    if let Some(previous) = self.dragging {
                        self.end_slider(previous);
                    }
                    self.dragging = Some(id);
                }
                self.write_slider(id, value.clamp(0.0, 1.0), begin);
            }
            Message::SliderReleased(id) => {
                if self.dragging == Some(id) {
                    self.end_slider(id);
                    self.dragging = None;
                }
            }
            Message::Preview(ratio) => self.preview_at(ratio),
        }
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let params = &self.model.params;
        let bucket = sensitivity_bucket(params.sensitivity.value());
        let slice_count = self
            .sample
            .as_ref()
            .and_then(|sample| sample.buckets.get(bucket))
            .map(|slices| slices.len())
            .unwrap_or(0);
        let standalone = self.model.shared.standalone.load(Ordering::Relaxed);
        let audition = self.model.shared.audition.load(Ordering::Relaxed);
        let play_label = if audition {
            "Stop"
        } else if standalone {
            "Play"
        } else {
            "Audition"
        };
        let transport = if self.snapshot.host_playing {
            format!("Host is playing at {:.1} BPM", self.snapshot.bpm)
        } else if audition {
            format!(
                "{} at {:.1} BPM",
                if standalone { "Playing" } else { "Audition" },
                self.snapshot.bpm
            )
        } else if standalone {
            "Press Play to start the loop.".to_string()
        } else {
            "Press play in the DAW, or Audition while it is stopped.".to_string()
        };

        let import = if self.model.import_open {
            button("Import")
        } else {
            button("Import").on_press(Message::Import)
        };
        let undo = button("Undo").on_press_maybe(self.model.undo_seed.map(|_| Message::Undo));

        let waveform = match &self.sample {
            Some(sample) => canvas::Canvas::new(Waveform {
                sample: Arc::clone(sample),
                bucket,
                pattern: self.snapshot.pattern,
            })
            .width(Length::Fill)
            .height(128)
            .into(),
            None => empty_waveform(),
        };

        let meter = self.model.shared.meter.load(Ordering::Relaxed);
        let db = if meter > 1.0e-8 {
            20.0 * meter.log10()
        } else {
            -80.0
        };
        let normalized = ((db + 60.0) / 60.0).clamp(0.0, 1.0);

        scrollable(
            column![
                text(crate::Sonomori::NAME).size(22),
                text("Import a sound, chop it on transients, and play a generated loop."),
                row![
                    import,
                    button("Randomize").on_press(Message::Randomize),
                    undo,
                    text(format!("Seed {}", params.seed.value())),
                ]
                .spacing(8)
                .align_y(Center),
                row![button(play_label).on_press(Message::ToggleAudition), text(transport)]
                    .spacing(8)
                    .align_y(Center),
                row![
                    text_input("or paste a file path", &self.model.path_input)
                        .on_input(Message::PathChanged)
                        .on_submit(Message::LoadPath)
                        .width(360.0),
                    button("Load path").on_press(Message::LoadPath),
                ]
                .spacing(8)
                .align_y(Center),
                text(status_line(
                    &self.model.shared,
                    self.sample.as_deref(),
                    slice_count,
                )),
                waveform,
                text("Pattern"),
                column((0..LANES).map(|lane| lane_block(lane, &self.snapshot))).spacing(4),
                row![
                    column(LEFT_SLIDERS.map(|id| self.slider(id)))
                        .spacing(6)
                        .width(Length::Fill),
                    column(RIGHT_SLIDERS.map(|id| self.slider(id)))
                        .spacing(6)
                        .width(Length::Fill),
                ]
                .spacing(16),
                row![
                    progress_bar(0.0..=1.0, normalized)
                        .length(Length::Fill)
                        .girth(12.0),
                    text(format!("{db:.1} dB")),
                ]
                .spacing(8)
                .align_y(Center),
                text(
                    "Click a slice to preview it. Randomize picks new slices and a new groove. The macros reshape that groove."
                )
                .size(12)
                .color(rgb(140, 144, 150)),
            ]
            .spacing(8)
            .padding(16)
            .width(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn slider(&self, id: SliderId) -> Element<'_, Message> {
        let params = &self.model.params;
        let standalone = self.model.shared.standalone.load(Ordering::Relaxed);
        let (label, normalized, value) = slider_state(params, id, standalone);
        column![
            row![text(label), text(value)].spacing(8),
            slider(0.0..=1.0, normalized, move |next| {
                Message::SliderChanged(id, next)
            })
            .step(0.001_f32)
            .on_release(Message::SliderReleased(id)),
        ]
        .spacing(2)
        .into()
    }

    fn refresh(&mut self) {
        self.poll_loads();
        self.poll_import();
        self.sample = self.model.shared.sample.load_full();
        self.snapshot = *self.model.snapshot_rx.read();
    }

    fn waiting_on_disk(&self) -> bool {
        self.model.import_open
            || matches!(*self.model.shared.status.lock(), LoadStatus::Loading { .. })
    }

    fn poll_loads(&mut self) {
        while let Ok(finished) = self.model.load_rx.try_recv() {
            if finished.id != self.model.active_load_id {
                continue;
            }
            match finished.result {
                Ok(data) => {
                    *self.model.params.sample_path.lock() = data.path.clone();
                    self.model.path_input = data.path.clone();
                    self.model.shared.sample.store(Some(Arc::new(data)));
                    *self.model.shared.status.lock() = LoadStatus::Idle;
                }
                Err(message) => {
                    *self.model.shared.status.lock() = LoadStatus::Failed { message };
                }
            }
        }
    }

    fn begin_load(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".to_string());
        *self.model.shared.status.lock() = LoadStatus::Loading { name };
        let id = self.model.next_load_id;
        self.model.next_load_id = self.model.next_load_id.wrapping_add(1);
        self.model.active_load_id = id;
        let tx = self.model.load_tx.clone();
        std::thread::spawn(move || {
            let result = crate::sample::load(&path);
            let _ = tx.send(LoadFinished { id, result });
        });
    }

    fn poll_import(&mut self) {
        while let Ok(path) = self.model.import_rx.try_recv() {
            self.model.import_open = false;
            if let Some(path) = path {
                self.begin_load(path);
            }
        }
    }

    fn preview_at(&mut self, ratio: f32) {
        let Some(sample) = &self.sample else {
            return;
        };
        if sample.len() == 0 {
            return;
        }
        let bucket = sensitivity_bucket(self.model.params.sensitivity.value());
        let Some(slices) = sample.buckets.get(bucket) else {
            return;
        };
        let frame = (ratio.clamp(0.0, 0.999) * sample.len() as f32) as usize;
        if let Some(index) = slices
            .iter()
            .position(|slice| frame >= slice.start && frame < slice.end)
        {
            let _ = self.model.preview_tx.push(index as u32);
        }
    }

    fn write_slider(&mut self, id: SliderId, value: f32, begin: bool) {
        let setter = self.ctx.nice_context.param_setter();
        let params = &self.model.params;
        match id {
            SliderId::Density => touch(&setter, &params.density, value, begin),
            SliderId::Syncopation => touch(&setter, &params.syncopation, value, begin),
            SliderId::Symmetry => touch(&setter, &params.symmetry, value, begin),
            SliderId::PatternVar => touch(&setter, &params.pattern_var, value, begin),
            SliderId::Swing => touch(&setter, &params.swing, value, begin),
            SliderId::Bars => touch(&setter, &params.bars, value, begin),
            SliderId::SoundVar => touch(&setter, &params.sound_var, value, begin),
            SliderId::Decay => touch(&setter, &params.decay, value, begin),
            SliderId::Pitch => touch(&setter, &params.pitch, value, begin),
            SliderId::Tone => touch(&setter, &params.tone, value, begin),
            SliderId::Sensitivity => touch(&setter, &params.sensitivity, value, begin),
            SliderId::Tempo => touch(&setter, &params.tempo, value, begin),
            SliderId::Output => touch(&setter, &params.output, value, begin),
        }
    }

    fn end_slider(&mut self, id: SliderId) {
        let setter = self.ctx.nice_context.param_setter();
        let params = &self.model.params;
        match id {
            SliderId::Density => release(&setter, &params.density),
            SliderId::Syncopation => release(&setter, &params.syncopation),
            SliderId::Symmetry => release(&setter, &params.symmetry),
            SliderId::PatternVar => release(&setter, &params.pattern_var),
            SliderId::Swing => release(&setter, &params.swing),
            SliderId::Bars => release(&setter, &params.bars),
            SliderId::SoundVar => release(&setter, &params.sound_var),
            SliderId::Decay => release(&setter, &params.decay),
            SliderId::Pitch => release(&setter, &params.pitch),
            SliderId::Tone => release(&setter, &params.tone),
            SliderId::Sensitivity => release(&setter, &params.sensitivity),
            SliderId::Tempo => release(&setter, &params.tempo),
            SliderId::Output => release(&setter, &params.output),
        }
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        if let Some(id) = self.dragging.take() {
            self.end_slider(id);
        }
    }
}

fn slider_state(
    params: &crate::SonomoriParams,
    id: SliderId,
    standalone: bool,
) -> (&'static str, f32, String) {
    match id {
        SliderId::Density => labeled("Density", &params.density),
        SliderId::Syncopation => labeled("Syncopation", &params.syncopation),
        SliderId::Symmetry => labeled("Symmetry", &params.symmetry),
        SliderId::PatternVar => labeled("Pattern variation", &params.pattern_var),
        SliderId::Swing => labeled("Swing", &params.swing),
        SliderId::Bars => labeled("Bars", &params.bars),
        SliderId::SoundVar => labeled("Sound variation", &params.sound_var),
        SliderId::Decay => labeled("Decay", &params.decay),
        SliderId::Pitch => labeled("Pitch", &params.pitch),
        SliderId::Tone => labeled("Tone", &params.tone),
        SliderId::Sensitivity => labeled("Slice sensitivity", &params.sensitivity),
        SliderId::Tempo => labeled(
            if standalone {
                "Tempo"
            } else {
                "Audition tempo"
            },
            &params.tempo,
        ),
        SliderId::Output => labeled("Output", &params.output),
    }
}

fn labeled(label: &'static str, param: &impl Param) -> (&'static str, f32, String) {
    let normalized = param.modulated_normalized_value();
    let value = param.normalized_value_to_string(normalized, true);
    (label, normalized, value)
}

fn touch(setter: &ParamSetter<'_>, param: &impl Param, value: f32, begin: bool) {
    if begin {
        setter.begin_set_parameter(param);
    }
    setter.set_parameter_normalized(param, value);
}

fn release(setter: &ParamSetter<'_>, param: &impl Param) {
    setter.end_set_parameter(param);
}

fn randomize(
    setter: &ParamSetter<'_>,
    params: &crate::SonomoriParams,
    undo_seed: &mut Option<i32>,
) {
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

fn empty_waveform() -> Element<'static, Message> {
    container(text("No sound loaded").size(14).color(rgb(140, 144, 150)))
        .width(Length::Fill)
        .height(110)
        .align_x(Center)
        .align_y(Center)
        .style(|_| container::Style {
            background: Some(panel_background()),
            border: iced::Border {
                radius: 4.0.into(),
                ..iced::Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

fn lane_block(lane: usize, snapshot: &Snapshot) -> Element<'static, Message> {
    let color = rgb(LANE_RGB[lane][0], LANE_RGB[lane][1], LANE_RGB[lane][2]);
    column![
        row![
            container(Space::new().width(10).height(10))
                .width(10)
                .height(10)
                .style(move |_| container::Style {
                    background: Some(iced::Background::Color(color)),
                    border: iced::Border {
                        radius: 2.0.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }),
            text(LANE_NAMES[lane]),
        ]
        .spacing(6)
        .align_y(Center),
        canvas::Canvas::new(LaneBar {
            velocity: snapshot.pattern.lanes[lane].velocity,
            steps: snapshot.pattern.steps,
            lane,
            playhead: snapshot.playhead,
        })
        .width(Length::Fill)
        .height(18),
    ]
    .spacing(2)
    .into()
}

fn panel_background() -> iced::Background {
    iced::Background::Color(rgb(22, 24, 28))
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

fn scale_color(channels: [u8; 3], factor: f32) -> Color {
    Color::from_rgb(
        channels[0] as f32 / 255.0 * factor,
        channels[1] as f32 / 255.0 * factor,
        channels[2] as f32 / 255.0 * factor,
    )
}

struct Waveform {
    sample: Arc<SampleData>,
    bucket: usize,
    pattern: Pattern,
}

impl canvas::Program<Message> for Waveform {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, frame.size(), rgb(22, 24, 28));
        let width = frame.width();
        let height = frame.height();
        if self.sample.len() == 0 || width < 2.0 {
            return vec![frame.into_geometry()];
        }

        let slices = self
            .sample
            .buckets
            .get(self.bucket)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let frames = self.sample.len() as f32;
        for (lane, lane_pattern) in self.pattern.lanes.iter().enumerate() {
            let Some(slice) = slices.get(lane_pattern.slice) else {
                continue;
            };
            let start_x = width * slice.start as f32 / frames;
            let end_x = width * slice.end as f32 / frames;
            frame.fill_rectangle(
                Point::new(start_x, 0.0),
                Size::new((end_x - start_x).max(1.0), height),
                scale_color(LANE_RGB[lane], 0.28),
            );
        }

        let mid_y = height * 0.5;
        let scale = if self.sample.display_peak > 0.0 {
            height * 0.46 / self.sample.display_peak
        } else {
            0.0
        };
        let columns = self.sample.peaks.len().max(1) as f32;
        let mut peaks = canvas::path::Builder::new();
        for (index, peak) in self.sample.peaks.iter().enumerate() {
            let x = width * index as f32 / columns;
            peaks.move_to(Point::new(x, mid_y - peak.max * scale));
            peaks.line_to(Point::new(x, mid_y - peak.min * scale));
        }
        frame.stroke(&peaks.build(), stroke(1.0, rgb(210, 214, 220)));

        let mut markers = canvas::path::Builder::new();
        for slice in slices {
            let x = width * slice.start as f32 / frames;
            markers.move_to(Point::new(x, 0.0));
            markers.line_to(Point::new(x, height));
        }
        frame.stroke(&markers.build(), stroke(1.0, rgba(255, 255, 255, 40)));

        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        _state: &mut Self::State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event else {
            return None;
        };
        let position = cursor.position_in(bounds)?;
        if bounds.width <= 1.0 {
            return None;
        }
        let ratio = (position.x / bounds.width).clamp(0.0, 0.999);
        Some(canvas::Action::publish(Message::Preview(ratio)).and_capture())
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

struct LaneBar {
    velocity: [f32; MAX_STEPS],
    steps: usize,
    lane: usize,
    playhead: Option<usize>,
}

impl canvas::Program<Message> for LaneBar {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let steps = self.steps.max(1);
        let cell = frame.width() / steps as f32;
        let height = frame.height();
        for step in 0..steps {
            let velocity = self.velocity.get(step).copied().unwrap_or(0.0);
            let color = if velocity > 0.0 {
                scale_color(LANE_RGB[self.lane], 0.35 + 0.65 * velocity)
            } else {
                rgb(38, 40, 46)
            };
            let x = cell * step as f32 + 1.0;
            let bar_width = (cell - 2.0).max(1.0);
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(bar_width, height), color);
            if self.playhead == Some(step) {
                frame.fill_rectangle(
                    Point::new(x + 1.0, 1.0),
                    Size::new((bar_width - 2.0).max(1.0), (height - 2.0).max(1.0)),
                    rgba(255, 255, 255, 70),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

fn stroke(width: f32, color: Color) -> Stroke<'static> {
    Stroke::default().with_width(width).with_color(color)
}

/// Wakes the editor while a file panel or decode is in flight.
///
/// The audio thread's poll flag stays clear in that case, and this backend
/// has no `time::every` subscription.
fn disk_wait() -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    use iced::futures::sink::SinkExt;

    iced::stream::channel(1, async |mut sender| {
        let (mut tick_tx, mut tick_rx) = iced::futures::channel::mpsc::channel(1);
        std::thread::spawn(move || {
            loop {
                if let Err(error) = tick_tx.try_send(())
                    && error.is_disconnected()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        while tick_rx.next().await.is_some() {
            if sender.send(Message::Poll).await.is_err() {
                break;
            }
        }
    })
    .boxed()
}
