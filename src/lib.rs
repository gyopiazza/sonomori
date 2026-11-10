mod editor;
mod engine;
mod features;
mod params;
mod pattern;
mod sample;
mod score;
mod session;
mod slice;
pub mod ui;

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use nice_plug::prelude::*;

use editor::SonomoriEditor;
use engine::{Audio, Engine, FrameInput, Preview, step_at};
use params::{SonomoriParams, gen_params};
use pattern::{GenParams, cache_key};
use score::{MAX_STEPS, Pattern};
use session::{BeatCursor, Clock, LoadStatus, METER_ACTIVE, Shared, Snapshot, beats_from_samples};
use slice::sensitivity_bucket;

const WINDOW_SIZE: nice_plug::editor::dpi::LogicalSize<f32> =
    nice_plug::editor::dpi::LogicalSize::new(960.0, 740.0);
const DEFAULT_SAMPLE_RATE: f32 = 44_100.0;
const REPAINT_SECONDS: f32 = 0.03;

fn repaint_every(sample_rate: f32) -> usize {
    ((sample_rate * REPAINT_SECONDS) as usize).max(1)
}
const RESIZE_HINT: ResizeHint = ResizeHint::resizable()
    .with_min_logical_size(nice_plug::editor::dpi::LogicalSize::new(760.0, 560.0));

pub struct Sonomori {
    params: Arc<SonomoriParams>,
    shared: Shared,
    editor_state: Arc<ui::EditorState>,
    repaint_notifier: ui::Repaint,
    initial_editor: Option<SonomoriEditor>,
    preview_rx: rtrb::Consumer<u32>,
    snapshot_tx: triple_buffer::Input<Snapshot>,
    engine: Engine,
    pattern: Pattern,
    pattern_key: Option<(Option<u64>, usize, GenParams)>,
    sample_rate: f32,
    beats: BeatCursor,
    attempted_path: String,
    repaint_interval: usize,
    repaint_accum: usize,
}

impl Default for Sonomori {
    fn default() -> Self {
        let params = Arc::new(SonomoriParams::default());
        let shared = Shared::new();
        let (preview_tx, preview_rx) = rtrb::RingBuffer::new(32);
        let (snapshot_tx, snapshot_rx) = triple_buffer::triple_buffer(&Snapshot::default());
        let editor = SonomoriEditor::new(params.clone(), shared.clone(), preview_tx, snapshot_rx);

        Self {
            params,
            shared,
            editor_state: ui::new_state(WINDOW_SIZE),
            repaint_notifier: ui::new_repaint(),
            initial_editor: Some(editor),
            preview_rx,
            snapshot_tx,
            engine: Engine::new(DEFAULT_SAMPLE_RATE),
            pattern: Pattern::default(),
            pattern_key: None,
            sample_rate: DEFAULT_SAMPLE_RATE,
            beats: BeatCursor::default(),
            attempted_path: String::new(),
            repaint_interval: repaint_every(DEFAULT_SAMPLE_RATE),
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
            Ok(data) => self.shared.store_sample(data),
            Err(message) => self.shared.set_status(LoadStatus::Failed { message }),
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

    type Editor = ui::PluginEditor;
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Self::Editor> {
        let editor = self.initial_editor.take()?;
        ui::open(
            self.editor_state.clone(),
            self.repaint_notifier.clone(),
            Self::NAME,
            RESIZE_HINT,
            editor,
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
        self.repaint_interval = repaint_every(buffer_config.sample_rate);
        self.maybe_load_persisted();
        true
    }

    fn reset(&mut self) {
        self.engine.reset();
        self.beats = BeatCursor::default();
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

        let mut preview_index = None;
        while let Ok(index) = self.preview_rx.pop() {
            preview_index = Some(index as usize);
        }

        let bucket = sensitivity_bucket(self.params.sensitivity.value());
        let generation = sample.map(|loaded| loaded.generation);
        let settings = gen_params(&self.params);
        let key = (generation, bucket, cache_key(&settings));
        if self.pattern_key != Some(key) {
            let slices = sample
                .and_then(|loaded| loaded.buckets.get(bucket))
                .map(|slices| slices.as_slice())
                .unwrap_or(&[]);
            self.pattern = pattern::generate(slices, &settings);
            self.pattern_key = Some(key);
        }

        let transport = context.transport();
        // Standalone always marks the transport as playing and ignores the tempo slider.
        let standalone = context.plugin_api() == PluginApi::Standalone;
        self.shared.standalone.store(standalone, Ordering::Relaxed);
        let audition = self.shared.audition.load(Ordering::Relaxed);
        let clock = Clock::from_transport(
            standalone,
            transport.playing,
            transport.tempo,
            self.params.tempo.value() as f64,
        );
        let playing = clock.host_playing || audition;

        let block = self.beats.begin(
            clock,
            audition,
            transport.pos_beats,
            transport.pos_samples,
            self.sample_rate as f64,
        );
        if block.disarm {
            self.engine.disarm_step();
        }

        let bpm = f64::from(clock.bpm);
        let start_beats = block.start;

        let swing = self.params.swing.value();
        let frames = buffer.samples();
        let channels = buffer.channels();
        let beats_per_sample = beats_from_samples(1.0, bpm, self.sample_rate as f64);
        let outputs = buffer.as_slice();
        let mut peak = 0.0f32;
        let preview = preview_index.and_then(|index| {
            let slice = sample?.buckets.get(bucket)?.get(index)?;
            Some(Preview {
                start: slice.start,
                end: slice.end,
                norm_gain: slice.norm_gain,
            })
        });
        let mut pending_preview = preview;
        let audio = sample.map(|loaded| Audio {
            sample_rate: loaded.sample_rate,
            left: &loaded.left,
            right: &loaded.right,
        });

        for frame in 0..frames {
            let gain = self.params.output.smoothed.next();
            let beats = start_beats + frame as f64 * beats_per_sample;
            let (left, right) = self.engine.frame(FrameInput {
                audio,
                pattern: &self.pattern,
                playing,
                beats,
                swing,
                preview: pending_preview.take(),
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
        self.beats.commit(&block, advanced);

        let held = self.shared.meter.load(Ordering::Relaxed);
        let decay = 0.25f32.powf(frames as f32 / (self.sample_rate * 0.15).max(1.0));
        let next_meter = if peak > held { peak } else { held * decay };
        self.shared.meter.store(next_meter, Ordering::Relaxed);

        let steps = self.pattern.steps.clamp(1, MAX_STEPS);
        let playhead = if playing {
            Some(step_at(start_beats, swing as f64, steps))
        } else {
            None
        };
        self.snapshot_tx.write(Snapshot {
            pattern: self.pattern,
            playhead,
            playing,
            clock,
        });

        if ui::is_open(&self.editor_state) && (playing || next_meter > METER_ACTIVE) {
            self.repaint_accum += frames;
            if self.repaint_accum >= self.repaint_interval {
                self.repaint_accum = 0;
                ui::request_repaint(&self.repaint_notifier);
            }
        }

        if playing || self.engine.any_active() {
            ProcessStatus::KeepAlive
        } else {
            ProcessStatus::Normal
        }
    }
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
