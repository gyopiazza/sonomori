//! State the audio thread and the editor both touch.
//!
//! The audio thread decides whether the host transport counts. The editor reads
//! that decision and writes the audition toggle.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwapOption;
use atomic_float::AtomicF32;
use parking_lot::Mutex;

use crate::sample::SampleData;
use crate::score::Pattern;

/// Below this the meter is treated as silent by both the process loop and the editor.
pub const METER_ACTIVE: f32 = 0.001;

#[derive(Clone)]
pub struct Shared {
    pub sample: Arc<ArcSwapOption<SampleData>>,
    pub status: Arc<Mutex<LoadStatus>>,
    pub audition: Arc<AtomicBool>,
    pub meter: Arc<AtomicF32>,
    /// The standalone wrapper reports a playing transport for the whole session.
    /// Playback and tempo then come from the editor instead of that transport.
    pub standalone: Arc<AtomicBool>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            sample: Arc::new(ArcSwapOption::empty()),
            status: Arc::new(Mutex::new(LoadStatus::Idle)),
            audition: Arc::new(AtomicBool::new(false)),
            meter: Arc::new(AtomicF32::new(0.0)),
            standalone: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_status(&self, status: LoadStatus) {
        *self.status.lock() = status;
    }

    pub fn store_sample(&self, data: SampleData) {
        self.sample.store(Some(Arc::new(data)));
        self.set_status(LoadStatus::Idle);
    }
}

pub enum LoadStatus {
    Idle,
    Loading { name: String },
    Failed { message: String },
}

/// Host transport after the standalone exception has been applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clock {
    pub host_playing: bool,
    pub bpm: f32,
}

impl Clock {
    pub fn from_transport(
        standalone: bool,
        transport_playing: bool,
        host_tempo: Option<f64>,
        tempo_param: f64,
    ) -> Self {
        let host_playing = transport_playing && !standalone;
        let bpm = if host_playing {
            host_tempo.unwrap_or(tempo_param)
        } else {
            tempo_param
        };
        Self {
            host_playing,
            bpm: bpm.clamp(20.0, 999.0) as f32,
        }
    }
}

/// Samples and quarter notes share one conversion, used both to read a host
/// position and to advance a free-running clock.
pub fn beats_from_samples(samples: f64, bpm: f64, sample_rate: f64) -> f64 {
    samples / sample_rate * bpm / 60.0
}

/// Beat position for one process block. `start` is the position of the first
/// frame. `disarm` is set when host transport or audition has just begun.
pub struct BeatBlock {
    pub start: f64,
    pub disarm: bool,
    host_playing: bool,
    audition: bool,
    has_position: bool,
}

/// Remembers the free-running clocks and whether the previous block was playing,
/// so a start can be told apart from a continuation.
#[derive(Clone, Copy)]
pub struct BeatCursor {
    audition_beats: f64,
    free_beats: f64,
    audition_was_on: bool,
    host_was_playing: bool,
}

impl Default for BeatCursor {
    fn default() -> Self {
        Self {
            audition_beats: 0.0,
            free_beats: 0.0,
            audition_was_on: false,
            host_was_playing: false,
        }
    }
}

impl BeatCursor {
    pub fn begin(
        &mut self,
        clock: Clock,
        audition: bool,
        pos_beats: Option<f64>,
        pos_samples: Option<i64>,
        sample_rate: f64,
    ) -> BeatBlock {
        let has_position = pos_beats.is_some() || pos_samples.is_some();
        let transport_started = clock.host_playing && !self.host_was_playing;
        let audition_started = audition && !self.audition_was_on && !clock.host_playing;
        if audition_started {
            self.audition_beats = 0.0;
        }
        if transport_started && !has_position {
            self.free_beats = 0.0;
        }
        self.host_was_playing = clock.host_playing;
        self.audition_was_on = audition;

        let bpm = f64::from(clock.bpm);
        let start = if clock.host_playing {
            if let Some(beats) = pos_beats {
                beats
            } else if let Some(samples) = pos_samples {
                beats_from_samples(samples as f64, bpm, sample_rate)
            } else {
                self.free_beats
            }
        } else {
            self.audition_beats
        };

        BeatBlock {
            start,
            disarm: transport_started || audition_started,
            host_playing: clock.host_playing,
            audition,
            has_position,
        }
    }

    pub fn commit(&mut self, block: &BeatBlock, advanced: f64) {
        if block.host_playing && !block.has_position {
            self.free_beats = block.start + advanced;
        } else if !block.host_playing && block.audition {
            self.audition_beats = block.start + advanced;
        }
    }
}

#[derive(Clone, Copy)]
pub struct Snapshot {
    pub pattern: Pattern,
    pub playhead: Option<usize>,
    pub playing: bool,
    pub clock: Clock,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            pattern: Pattern::default(),
            playhead: None,
            playing: false,
            clock: Clock {
                host_playing: false,
                bpm: 120.0,
            },
        }
    }
}
