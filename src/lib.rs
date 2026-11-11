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
use session::{
    BeatCursor, Clock, LoadStatus, METER_ACTIVE, Shared, Snapshot, beats_from_samples,
};
use slice::sensitivity_bucket;

pub(crate) const WINDOW_SIZE: nice_plug::editor::dpi::LogicalSize<f32> =
    nice_plug::editor::dpi::LogicalSize::new(960.0, 960.0);
pub(crate) const MIN_WINDOW_SIZE: nice_plug::editor::dpi::LogicalSize<f32> =
    nice_plug::editor::dpi::LogicalSize::new(760.0, 960.0);
const DEFAULT_SAMPLE_RATE: f32 = 44_100.0;
/// How often a stopped, decaying meter may wake the editor. While the
/// transport is running the editor wakes on the playhead step instead.
const METER_SECONDS: f32 = 0.10;

fn samples_for(sample_rate: f32, seconds: f32) -> usize {
    ((sample_rate * seconds) as usize).max(1)
}
const RESIZE_HINT: ResizeHint = ResizeHint::resizable().with_min_logical_size(MIN_WINDOW_SIZE);

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
    /// Bumps when the pattern is rebuilt, so the editor can wake without a
    /// byte-wise compare of the score on the audio thread.
    pattern_serial: u64,
    ui_view: UiView,
    meter_interval: usize,
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
            pattern_serial: 0,
            ui_view: UiView::default(),
            meter_interval: samples_for(DEFAULT_SAMPLE_RATE, METER_SECONDS),
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
        self.meter_interval = samples_for(buffer_config.sample_rate, METER_SECONDS);
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
            self.pattern_serial = self.pattern_serial.wrapping_add(1);
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
        let next_view = UiView {
            step: playhead,
            host_playing: clock.host_playing,
            audition,
            standalone,
            bpm_tenth: (f32::from(clock.bpm) * 10.0).round() as i16,
            pattern: self.pattern_serial,
            meter: meter_bucket(next_meter),
        };
        // The picture only changes when the step, transport, pattern, or a
        // stopped meter moves. Copying the score into the snapshot and waking
        // the window on every block redraws an unchanged editor.
        match ui_wake(self.ui_view, next_view, self.repaint_accum, self.meter_interval) {
            UiWake::Now => {
                if self.ui_view.musical_ne(next_view) {
                    self.snapshot_tx.write(Snapshot {
                        pattern: self.pattern,
                        playhead,
                        clock,
                    });
                }
                self.ui_view = next_view;
                self.repaint_accum = 0;
                if ui::is_open(&self.editor_state) {
                    ui::request_repaint(&self.repaint_notifier);
                }
            }
            UiWake::Wait => self.repaint_accum += frames,
            UiWake::Idle => self.repaint_accum = 0,
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

/// What the open editor is currently showing. Compared on the audio thread
/// so a steady loop does not wake the window on every block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UiView {
    step: Option<usize>,
    host_playing: bool,
    audition: bool,
    standalone: bool,
    bpm_tenth: i16,
    pattern: u64,
    meter: u16,
}

impl Default for UiView {
    fn default() -> Self {
        Self {
            step: None,
            host_playing: false,
            audition: false,
            standalone: false,
            bpm_tenth: 0,
            pattern: 0,
            meter: 0,
        }
    }
}

impl UiView {
    fn playing(self) -> bool {
        self.host_playing || self.audition
    }

    fn musical_ne(self, other: Self) -> bool {
        self.step != other.step
            || self.host_playing != other.host_playing
            || self.audition != other.audition
            || self.standalone != other.standalone
            || self.bpm_tenth != other.bpm_tenth
            || self.pattern != other.pattern
    }
}

enum UiWake {
    Now,
    Wait,
    Idle,
}

fn meter_bucket(meter: f32) -> u16 {
    if meter < METER_ACTIVE {
        return 0;
    }
    (meter.clamp(0.0, 1.0) * 32.0) as u16
}

fn ui_wake(previous: UiView, next: UiView, elapsed: usize, meter_interval: usize) -> UiWake {
    if previous.musical_ne(next) {
        return UiWake::Now;
    }
    // A running loop samples the meter on the next step. A stopped meter
    // still falls, but only as fast as the bucket interval.
    if previous.meter != next.meter && !next.playing() {
        if elapsed >= meter_interval {
            UiWake::Now
        } else {
            UiWake::Wait
        }
    } else {
        UiWake::Idle
    }
}

nice_export_clap!(Sonomori);
nice_export_vst3!(Sonomori);

#[cfg(test)]
mod ui_wake_tests {
    use super::*;

    fn view() -> UiView {
        UiView {
            step: None,
            host_playing: false,
            audition: false,
            standalone: true,
            bpm_tenth: 1200,
            pattern: 1,
            meter: 0,
        }
    }

    #[test]
    fn a_step_change_wakes_immediately() {
        let mut previous = view();
        previous.audition = true;
        previous.step = Some(0);
        let mut next = previous;
        next.step = Some(1);
        next.meter = 20;
        assert!(matches!(
            ui_wake(previous, next, 0, 1_000),
            UiWake::Now
        ));
    }

    #[test]
    fn meter_motion_while_playing_does_not_wake() {
        let mut previous = view();
        previous.audition = true;
        previous.step = Some(3);
        previous.meter = 10;
        let mut next = previous;
        next.meter = 4;
        assert!(matches!(
            ui_wake(previous, next, 10_000, 100),
            UiWake::Idle
        ));
    }

    #[test]
    fn a_falling_meter_waits_out_the_interval() {
        let previous = view();
        let mut next = previous;
        next.meter = 8;
        assert!(matches!(ui_wake(previous, next, 10, 1_000), UiWake::Wait));
        assert!(matches!(
            ui_wake(previous, next, 1_000, 1_000),
            UiWake::Now
        ));
    }

    #[test]
    fn silence_and_full_scale_land_in_distinct_buckets() {
        assert_eq!(meter_bucket(0.0), 0);
        assert!(meter_bucket(1.0) > meter_bucket(0.1));
        assert_eq!(meter_bucket(4.0), meter_bucket(1.0));
    }
}
