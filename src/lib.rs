mod editor;
mod engine;
mod pattern;
mod sample;
mod slice;

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwapOption;
use atomic_float::AtomicF32;
use nice_plug::prelude::*;
use nice_plug_iced::iced::PollSubNotifier;
use nice_plug_iced::{
    IcedEditor, IcedEditorState, IcedNiceSettings, application, create_iced_editor,
};
use parking_lot::Mutex;

use editor::{EditorModel, Gui};
use engine::{Engine, FrameInput, step_at};
use pattern::{GenParams, MAX_STEPS, Pattern};
use sample::SampleData;
use slice::sensitivity_bucket;

const WINDOW_SIZE: nice_plug::editor::dpi::LogicalSize<f32> =
    nice_plug::editor::dpi::LogicalSize::new(960.0, 740.0);
const RESIZE_HINT: ResizeHint = ResizeHint::resizable()
    .with_min_logical_size(nice_plug::editor::dpi::LogicalSize::new(760.0, 560.0));

pub struct Sonomori {
    params: Arc<SonomoriParams>,
    shared: Shared,
    editor_state: Arc<IcedEditorState>,
    repaint_notifier: PollSubNotifier,
    editor_model: Option<EditorModel>,
    preview_rx: rtrb::Consumer<u32>,
    snapshot_tx: triple_buffer::Input<Snapshot>,
    engine: Engine,
    pattern: Pattern,
    pattern_key: Option<PatternKey>,
    sample_rate: f32,
    audition_beats: f64,
    free_beats: f64,
    audition_was_on: bool,
    host_was_playing: bool,
    attempted_path: String,
    repaint_interval: usize,
    repaint_accum: usize,
}

#[derive(Params)]
struct SonomoriParams {
    #[id = "density"]
    pub density: FloatParam,
    #[id = "syncopation"]
    pub syncopation: FloatParam,
    #[id = "symmetry"]
    pub symmetry: FloatParam,
    #[id = "pattern_var"]
    pub pattern_var: FloatParam,
    #[id = "sound_var"]
    pub sound_var: FloatParam,
    #[id = "swing"]
    pub swing: FloatParam,
    #[id = "decay"]
    pub decay: FloatParam,
    #[id = "pitch"]
    pub pitch: IntParam,
    #[id = "tone"]
    pub tone: FloatParam,
    #[id = "sensitivity"]
    pub sensitivity: FloatParam,
    #[id = "bars"]
    pub bars: IntParam,
    #[id = "tempo"]
    pub tempo: FloatParam,
    #[id = "output"]
    pub output: FloatParam,
    #[id = "seed"]
    pub seed: IntParam,

    /// Restored with the DAW project so the imported file reloads next session.
    #[persist = "sample_path"]
    pub sample_path: Mutex<String>,
}

#[derive(Clone)]
pub(crate) struct Shared {
    pub sample: Arc<ArcSwapOption<SampleData>>,
    pub status: Arc<Mutex<LoadStatus>>,
    pub audition: Arc<AtomicBool>,
    pub meter: Arc<AtomicF32>,
    /// The standalone wrapper reports a playing transport for the whole session.
    /// Playback and tempo then come from the editor instead of that transport.
    pub standalone: Arc<AtomicBool>,
}

pub(crate) enum LoadStatus {
    Idle,
    Loading { name: String },
    Failed { message: String },
}

#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    pub pattern: Pattern,
    pub playhead: Option<usize>,
    #[allow(dead_code)]
    pub playing: bool,
    pub host_playing: bool,
    pub bpm: f32,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            pattern: Pattern::default(),
            playhead: None,
            playing: false,
            host_playing: false,
            bpm: 120.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PatternKey {
    seed: i32,
    density: i32,
    sync: i32,
    symmetry: i32,
    sound_var: i32,
    pattern_var: i32,
    decay: i32,
    pitch: i32,
    tone: i32,
    bars: i32,
    bucket: usize,
    generation: u64,
}

impl Default for SonomoriParams {
    fn default() -> Self {
        Self {
            density: percent_param("Density", 0.62),
            syncopation: bipolar_param("Syncopation", 0.1),
            symmetry: percent_param("Symmetry", 0.55),
            pattern_var: percent_param("Pattern Variation", 0.2),
            sound_var: percent_param("Sound Variation", 0.25),
            swing: FloatParam::new(
                "Swing",
                0.5,
                FloatRange::Linear {
                    min: 0.5,
                    max: 0.75,
                },
            )
            .with_step_size(0.01)
            .with_value_to_string(Arc::new(|value| format!("{:.0}%", value * 100.0)))
            .with_string_to_value(Arc::new(parse_ratio)),
            decay: percent_param("Decay", 0.55),
            pitch: IntParam::new("Pitch", 0, IntRange::Linear { min: -12, max: 12 })
                .with_unit(" st"),
            tone: percent_param("Tone", 0.78),
            sensitivity: percent_param("Sensitivity", 0.45),
            bars: IntParam::new("Bars", 1, IntRange::Linear { min: 1, max: 2 }),
            tempo: FloatParam::new(
                "Audition BPM",
                120.0,
                FloatRange::Linear {
                    min: 40.0,
                    max: 240.0,
                },
            )
            .with_step_size(1.0)
            .with_unit(" BPM")
            .with_value_to_string(formatters::v2s_f32_rounded(0)),
            output: FloatParam::new(
                "Output",
                util::db_to_gain(0.0),
                FloatRange::gain_range(-24.0, 6.0),
            )
            .with_smoother(SmoothingStyle::Logarithmic(30.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            seed: IntParam::new(
                "Seed",
                1,
                IntRange::Linear {
                    min: 1,
                    max: 999_999,
                },
            ),
            sample_path: Mutex::new(String::new()),
        }
    }
}

fn percent_param(name: &'static str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_step_size(0.01)
        .with_value_to_string(Arc::new(|value| format!("{:.0}%", value * 100.0)))
        .with_string_to_value(Arc::new(parse_ratio))
}

fn bipolar_param(name: &'static str, default: f32) -> FloatParam {
    FloatParam::new(
        name,
        default,
        FloatRange::Linear {
            min: -1.0,
            max: 1.0,
        },
    )
    .with_step_size(0.01)
    .with_value_to_string(Arc::new(|value| format!("{:+.0}", value * 100.0)))
    .with_string_to_value(Arc::new(|text| {
        let trimmed = text.trim().trim_end_matches('%').trim();
        trimmed.parse::<f32>().ok().map(|value| {
            if value.abs() > 1.5 {
                value / 100.0
            } else {
                value
            }
        })
    }))
}

fn parse_ratio(text: &str) -> Option<f32> {
    let trimmed = text.trim().trim_end_matches('%').trim();
    let value = trimmed.parse::<f32>().ok()?;
    Some(if value > 1.5 { value / 100.0 } else { value })
}

impl Default for Sonomori {
    fn default() -> Self {
        let params = Arc::new(SonomoriParams::default());
        let shared = Shared {
            sample: Arc::new(ArcSwapOption::empty()),
            status: Arc::new(Mutex::new(LoadStatus::Idle)),
            audition: Arc::new(AtomicBool::new(false)),
            meter: Arc::new(AtomicF32::new(0.0)),
            standalone: Arc::new(AtomicBool::new(false)),
        };
        let (preview_tx, preview_rx) = rtrb::RingBuffer::new(32);
        let (snapshot_tx, snapshot_rx) = triple_buffer::triple_buffer(&Snapshot::default());
        let editor_model =
            EditorModel::new(params.clone(), shared.clone(), preview_tx, snapshot_rx);

        Self {
            params,
            shared,
            editor_state: IcedEditorState::from_size(WINDOW_SIZE, 1.0),
            repaint_notifier: PollSubNotifier::new(),
            editor_model: Some(editor_model),
            preview_rx,
            snapshot_tx,
            engine: Engine::new(44_100.0),
            pattern: Pattern::default(),
            pattern_key: None,
            sample_rate: 44_100.0,
            audition_beats: 0.0,
            free_beats: 0.0,
            audition_was_on: false,
            host_was_playing: false,
            attempted_path: String::new(),
            repaint_interval: 1_323,
            repaint_accum: 0,
        }
    }
}

impl Sonomori {
    fn maybe_load_persisted(&mut self) {
        let path = self.params.sample_path.lock().clone();
        if path.is_empty() {
            return;
        }
        let current = self.shared.sample.load_full();
        if current.as_ref().is_some_and(|sample| sample.path == path) {
            return;
        }
        if self.attempted_path == path {
            return;
        }
        self.attempted_path = path.clone();
        match sample::load(Path::new(&path)) {
            Ok(data) => {
                self.shared.sample.store(Some(Arc::new(data)));
                *self.shared.status.lock() = LoadStatus::Idle;
            }
            Err(message) => {
                *self.shared.status.lock() = LoadStatus::Failed { message };
            }
        }
    }
}

impl Plugin for Sonomori {
    const NAME: &'static str = "Sonomori";
    const VENDOR: &'static str = "Sonomori";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: None,
            main_output_channels: NonZeroU32::new(2),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames::const_default(),
        },
        AudioIOLayout {
            main_input_channels: None,
            main_output_channels: NonZeroU32::new(1),
            aux_input_ports: &[],
            aux_output_ports: &[],
            names: PortNames::const_default(),
        },
    ];

    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type Editor = IcedEditor;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        let model = self.editor_model.take()?;
        create_iced_editor(
            self.editor_state.clone(),
            model,
            self.repaint_notifier.clone(),
            IcedNiceSettings::new()
                .with_tile(Self::NAME)
                .with_resize_hint(RESIZE_HINT),
            |model, ctx| {
                Ok(application(model, ctx, Gui::new, Gui::update, Gui::view)
                    .theme(Gui::theme)
                    .scale_factor(Gui::scale_factor)
                    .subscription(Gui::subscription)
                    .run())
            },
        )
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl ActivateContext<Self>,
    ) -> bool {
        self.sample_rate = buffer_config.sample_rate;
        self.engine.set_sample_rate(buffer_config.sample_rate);
        self.shared.standalone.store(
            context.plugin_api() == PluginApi::Standalone,
            Ordering::Relaxed,
        );
        self.repaint_interval = ((buffer_config.sample_rate * 0.03) as usize).max(1);
        self.maybe_load_persisted();
        true
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.audition_beats = 0.0;
        self.free_beats = 0.0;
        self.audition_was_on = false;
        self.host_was_playing = false;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let guard = self.shared.sample.load();
        let sample = match &*guard {
            Some(loaded) => Some(loaded.as_ref()),
            None => None,
        };

        let mut preview = None;
        while let Ok(index) = self.preview_rx.pop() {
            preview = Some(index as usize);
        }

        let bucket = sensitivity_bucket(self.params.sensitivity.value());
        let generation = sample.map(|loaded| loaded.generation).unwrap_or(0);
        let previous_key = self.pattern_key;
        let key = PatternKey {
            seed: self.params.seed.value(),
            density: quantize(self.params.density.value()),
            sync: quantize(self.params.syncopation.value()),
            symmetry: quantize(self.params.symmetry.value()),
            sound_var: quantize(self.params.sound_var.value()),
            pattern_var: quantize(self.params.pattern_var.value()),
            decay: quantize(self.params.decay.value()),
            pitch: self.params.pitch.value(),
            tone: quantize(self.params.tone.value()),
            bars: self.params.bars.value().clamp(1, 2),
            bucket,
            generation,
        };
        if self.pattern_key != Some(key) {
            let slices = sample
                .and_then(|loaded| loaded.buckets.get(bucket))
                .map(|slices| slices.as_slice())
                .unwrap_or(&[]);
            self.pattern = pattern::generate(
                slices,
                &GenParams {
                    seed: key.seed.max(0) as u32,
                    density: self.params.density.value(),
                    syncopation: self.params.syncopation.value(),
                    symmetry: self.params.symmetry.value(),
                    sound_var: self.params.sound_var.value(),
                    pattern_var: self.params.pattern_var.value(),
                    decay: self.params.decay.value(),
                    pitch: self.params.pitch.value() as f32,
                    tone: self.params.tone.value(),
                    bars: key.bars as usize,
                    bucket,
                },
            );
            self.pattern_key = Some(key);
        }

        let transport = context.transport();
        // Standalone always marks the transport as playing and ignores the tempo slider.
        let standalone = context.plugin_api() == PluginApi::Standalone;
        self.shared.standalone.store(standalone, Ordering::Relaxed);
        let host_playing = transport.playing && !standalone;
        let audition = self.shared.audition.load(Ordering::Relaxed);
        let playing = host_playing || audition;
        let tempo_param = self.params.tempo.value() as f64;
        let bpm = if host_playing {
            transport.tempo.unwrap_or(tempo_param)
        } else {
            tempo_param
        }
        .clamp(20.0, 999.0);

        let has_position = transport.pos_beats.is_some() || transport.pos_samples.is_some();
        let transport_started = host_playing && !self.host_was_playing;
        let audition_started = audition && !self.audition_was_on && !host_playing;
        if transport_started || audition_started {
            self.engine.disarm_step();
        }
        if audition_started {
            self.audition_beats = 0.0;
        }
        if transport_started && !has_position {
            self.free_beats = 0.0;
        }
        self.host_was_playing = host_playing;
        self.audition_was_on = audition;

        let start_beats = if host_playing {
            if let Some(beats) = transport.pos_beats {
                beats
            } else if let Some(samples) = transport.pos_samples {
                samples as f64 / self.sample_rate as f64 * bpm / 60.0
            } else {
                self.free_beats
            }
        } else {
            self.audition_beats
        };

        let swing = self.params.swing.value();
        let frames = buffer.samples();
        let channels = buffer.channels();
        let beats_per_sample = bpm / 60.0 / self.sample_rate as f64;
        let outputs = buffer.as_slice();
        let mut peak = 0.0f32;
        let mut pending_preview = preview;

        for frame in 0..frames {
            let gain = self.params.output.smoothed.next();
            let beats = start_beats + frame as f64 * beats_per_sample;
            let (left, right) = self.engine.frame(FrameInput {
                sample,
                pattern: &self.pattern,
                playing,
                beats,
                swing,
                preview_slice: pending_preview.take(),
                gain,
            });
            peak = peak.max(left.abs()).max(right.abs());
            if channels == 1 {
                outputs[0][frame] = 0.5 * (left + right);
            } else if channels >= 2 {
                outputs[0][frame] = left;
                outputs[1][frame] = right;
                for extra in outputs.iter_mut().skip(2) {
                    extra[frame] = 0.0;
                }
            }
        }

        let advanced = frames as f64 * beats_per_sample;
        if host_playing && !has_position {
            self.free_beats = start_beats + advanced;
        } else if !host_playing && audition {
            self.audition_beats = start_beats + advanced;
        }

        let held = self.shared.meter.load(Ordering::Relaxed);
        let decay = 0.25f32.powf(frames as f32 / (self.sample_rate * 0.15).max(1.0));
        let next_meter = if peak > held { peak } else { held * decay };
        self.shared.meter.store(next_meter, Ordering::Relaxed);

        let steps = self.pattern.steps.clamp(1, MAX_STEPS);
        let playhead = if playing {
            Some(step_at(start_beats, swing as f64, steps))
        } else {
            self.engine.current_step
        };
        self.snapshot_tx.write(Snapshot {
            pattern: self.pattern,
            playhead,
            playing,
            host_playing,
            bpm: bpm as f32,
        });

        let pattern_changed = previous_key != self.pattern_key;
        if self.editor_state.is_open() && (playing || next_meter > 0.001 || pattern_changed) {
            self.repaint_accum += frames;
            if self.repaint_accum >= self.repaint_interval {
                self.repaint_accum = 0;
                self.repaint_notifier.notify();
            }
        }

        if playing || self.engine.any_active() {
            ProcessStatus::KeepAlive
        } else {
            ProcessStatus::Normal
        }
    }
}

fn quantize(value: f32) -> i32 {
    (value * 1000.0).round() as i32
}

impl ClapPlugin for Sonomori {
    const CLAP_ID: &'static str = "com.sonomori.sonomori";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Slice an imported sound and play a generated loop");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Sampler,
        ClapFeature::Stereo,
        ClapFeature::Mono,
    ];
}

impl Vst3Plugin for Sonomori {
    const VST3_CLASS_ID: [u8; 16] = *b"SonomoriLoopGen1";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[
        Vst3SubCategory::Instrument,
        Vst3SubCategory::Sampler,
        Vst3SubCategory::Drum,
    ];
}

nice_export_clap!(Sonomori);
nice_export_vst3!(Sonomori);
