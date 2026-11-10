//! The loop the generator writes and the engine plays.
//!
//! Lanes, step count, and the swing and bar limits live here so a change to the
//! grid shows up in one place for playback, generation, and the parameter ranges.

/// Fraction of an eighth note given to the first sixteenth. Higher delays the off-sixteenth.
pub const SWING_MIN: f32 = 0.5;
pub const SWING_MAX: f32 = 0.75;

pub const BARS_MIN: usize = 1;
pub const BARS_MAX: usize = 2;

pub const BEATS_PER_BAR: usize = 4;
pub const STEPS_PER_BEAT: usize = 4;
pub const STEPS_PER_BAR: usize = BEATS_PER_BAR * STEPS_PER_BEAT;
pub const HALF_BAR: usize = STEPS_PER_BAR / 2;
pub const MAX_STEPS: usize = STEPS_PER_BAR * BARS_MAX;

/// Playback interpolates between a sample and the one after it, so a slice
/// shorter than this has nothing to play.
pub const MIN_SLICE_SAMPLES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Lane {
    Low = 0,
    Mid = 1,
    High = 2,
    Extra = 3,
}

impl Lane {
    pub const ALL: [Lane; 4] = [Lane::Low, Lane::Mid, Lane::High, Lane::Extra];

    pub const fn index(self) -> usize {
        self as usize
    }
}

pub const LANES: usize = Lane::ALL.len();

#[cfg(test)]
pub const LANE_LOW: usize = Lane::Low as usize;
#[cfg(test)]
pub const LANE_MID: usize = Lane::Mid as usize;
#[cfg(test)]
pub const LANE_HIGH: usize = Lane::High as usize;
#[cfg(test)]
pub const LANE_EXTRA: usize = Lane::Extra as usize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LanePattern {
    pub velocity: [f32; MAX_STEPS],
    pub slice: usize,
    /// Sample range copied from the chosen slice, so playback does not look the slice up again.
    pub start: usize,
    pub end: usize,
    pub norm_gain: f32,
    pub pitch: f32,
    pub decay: f32,
    pub tone: f32,
    pub gain: f32,
    pub pan: f32,
}

impl Default for LanePattern {
    fn default() -> Self {
        Self {
            velocity: [0.0; MAX_STEPS],
            slice: 0,
            start: 0,
            end: 0,
            norm_gain: 1.0,
            pitch: 0.0,
            decay: 0.55,
            tone: 0.8,
            gain: 1.0,
            pan: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pattern {
    pub lanes: [LanePattern; LANES],
    pub steps: usize,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            lanes: [LanePattern::default(); LANES],
            steps: STEPS_PER_BAR,
        }
    }
}
