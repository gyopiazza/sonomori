//! Sample-accurate loop player.
//!
//! Four monophonic lanes plus one preview voice. Timing follows the host
//! timeline when it is playing, in sixteenth notes, with a swing ratio that
//! delays every other sixteenth.

use crate::pattern::{Pattern, LANES, MAX_STEPS};
use crate::sample::SampleData;

const VOICES: usize = LANES + 1;
const PREVIEW: usize = LANES;
const ATTACK_SAMPLES: u32 = 16;
const END_FADE_SAMPLES: f64 = 64.0;

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    pos: f64,
    increment: f64,
    end: usize,
    env: f32,
    attack_left: u32,
    decay: f32,
    gain: f32,
    pan_l: f32,
    pan_r: f32,
    lp_l: f32,
    lp_r: f32,
    /// One-pole lowpass coefficient. 1 follows the input exactly.
    lp_a: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            active: false,
            pos: 0.0,
            increment: 1.0,
            end: 0,
            env: 0.0,
            attack_left: 0,
            decay: 1.0,
            gain: 0.0,
            pan_l: 0.707,
            pan_r: 0.707,
            lp_l: 0.0,
            lp_r: 0.0,
            lp_a: 1.0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    fn process(&mut self, input: f32) -> f32 {
        let output = input - self.x1 + 0.995 * self.y1;
        self.x1 = input;
        self.y1 = output;
        if output.abs() < 1.0e-20 {
            0.0
        } else {
            output
        }
    }
}

pub struct Engine {
    sample_rate: f32,
    voices: [Voice; VOICES],
    last_step: Option<usize>,
    stop_env: f32,
    dc_l: DcBlock,
    dc_r: DcBlock,
    pub current_step: Option<usize>,
}

pub struct FrameInput<'a> {
    pub sample: Option<&'a SampleData>,
    pub pattern: &'a Pattern,
    pub playing: bool,
    pub beats: f64,
    pub swing: f32,
    pub preview_slice: Option<usize>,
    pub gain: f32,
}

impl Engine {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            voices: [Voice::default(); VOICES],
            last_step: None,
            stop_env: 0.0,
            dc_l: DcBlock::default(),
            dc_r: DcBlock::default(),
            current_step: None,
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
    }

    pub fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
        self.last_step = None;
        self.stop_env = 0.0;
        self.dc_l = DcBlock::default();
        self.dc_r = DcBlock::default();
        self.current_step = None;
    }

    /// Forget the last triggered step so the next playing sample fires again.
    pub fn disarm_step(&mut self) {
        self.last_step = None;
    }

    pub fn any_active(&self) -> bool {
        self.voices.iter().any(|voice| voice.active) || self.stop_env > 0.0
    }

    pub fn frame(&mut self, input: FrameInput<'_>) -> (f32, f32) {
        let fade_step = 1.0 / (0.012 * self.sample_rate).max(1.0);
        if input.playing {
            self.stop_env = (self.stop_env + fade_step).min(1.0);
        } else {
            self.stop_env = (self.stop_env - fade_step).max(0.0);
            if self.stop_env == 0.0 {
                for voice in &mut self.voices[..LANES] {
                    voice.active = false;
                }
                self.last_step = None;
            }
        }

        let steps = input.pattern.steps.clamp(1, MAX_STEPS);
        let step = step_at(input.beats, input.swing as f64, steps);
        if input.playing {
            let changed = self.last_step != Some(step);
            if changed {
                self.trigger_step(input.sample, input.pattern, step);
                self.last_step = Some(step);
            }
            self.current_step = Some(step);
        } else {
            self.current_step = None;
        }

        if let Some(slice_index) = input.preview_slice {
            self.trigger_preview(input.sample, input.pattern.bucket, slice_index);
        }

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        if let Some(sample) = input.sample {
            for (index, voice) in self.voices.iter_mut().enumerate() {
                let (voice_l, voice_r) = voice.render(sample);
                if index == PREVIEW {
                    left += voice_l;
                    right += voice_r;
                } else {
                    left += voice_l * self.stop_env;
                    right += voice_r * self.stop_env;
                }
            }
        }

        left = soft_clip(self.dc_l.process(left) * input.gain);
        right = soft_clip(self.dc_r.process(right) * input.gain);
        (left, right)
    }

    fn trigger_step(&mut self, sample: Option<&SampleData>, pattern: &Pattern, step: usize) {
        let Some(sample) = sample else {
            return;
        };
        let Some(slices) = sample.buckets.get(pattern.bucket) else {
            return;
        };
        for lane in 0..LANES {
            let velocity = pattern.lanes[lane].velocity[step];
            if velocity <= 0.0 {
                continue;
            }
            let Some(slice) = slices.get(pattern.lanes[lane].slice) else {
                continue;
            };
            self.voices[lane].trigger(
                sample,
                slice.start,
                slice.end,
                pattern.lanes[lane].pitch,
                pattern.lanes[lane].decay,
                pattern.lanes[lane].tone,
                pattern.lanes[lane].gain * slice.norm_gain * velocity,
                pattern.lanes[lane].pan,
                self.sample_rate,
            );
        }
    }

    fn trigger_preview(&mut self, sample: Option<&SampleData>, bucket: usize, slice_index: usize) {
        let Some(sample) = sample else {
            return;
        };
        let Some(slice) = sample
            .buckets
            .get(bucket)
            .and_then(|slices| slices.get(slice_index))
        else {
            return;
        };
        self.voices[PREVIEW].trigger(
            sample,
            slice.start,
            slice.end,
            0.0,
            0.7,
            1.0,
            slice.norm_gain,
            0.0,
            self.sample_rate,
        );
    }
}

impl Voice {
    fn trigger(
        &mut self,
        sample: &SampleData,
        start: usize,
        end: usize,
        pitch: f32,
        decay: f32,
        tone: f32,
        gain: f32,
        pan: f32,
        host_rate: f32,
    ) {
        let end = end.min(sample.len());
        if end.saturating_sub(start) < 2 || host_rate <= 0.0 || sample.sample_rate <= 0.0 {
            self.active = false;
            return;
        }
        let ratio = 2.0f64.powf((pitch as f64) / 12.0);
        self.pos = start as f64;
        self.end = end;
        self.increment = (sample.sample_rate as f64 / host_rate as f64) * ratio;
        self.increment = self.increment.clamp(0.05, 16.0);
        self.env = 0.0;
        self.attack_left = ATTACK_SAMPLES;
        let slice_seconds = (end - start) as f32 / sample.sample_rate;
        let seconds = (0.045 + decay * decay * 1.35).min(slice_seconds.max(0.045));
        let samples = (seconds * host_rate).max(1.0);
        self.decay = 0.001f32.powf(1.0 / samples);
        self.gain = gain;
        let pan = pan.clamp(-1.0, 1.0);
        let angle = (pan + 1.0) * 0.25 * std::f32::consts::PI;
        self.pan_l = angle.cos();
        self.pan_r = angle.sin();
        self.lp_a = lowpass_coef(tone, host_rate);
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.active = true;
    }

    fn render(&mut self, sample: &SampleData) -> (f32, f32) {
        if !self.active {
            return (0.0, 0.0);
        }
        let limit = self.end.min(sample.len());
        if self.pos + 1.0 >= limit as f64 {
            self.active = false;
            return (0.0, 0.0);
        }

        let index = self.pos as usize;
        let frac = (self.pos - index as f64) as f32;
        let next = index + 1;
        if next >= sample.left.len() || next >= sample.right.len() {
            self.active = false;
            return (0.0, 0.0);
        }
        let left = sample.left[index] * (1.0 - frac) + sample.left[next] * frac;
        let right = sample.right[index] * (1.0 - frac) + sample.right[next] * frac;
        self.pos += self.increment;

        if self.attack_left > 0 {
            self.attack_left -= 1;
            self.env = 1.0 - self.attack_left as f32 / ATTACK_SAMPLES as f32;
        } else {
            self.env *= self.decay;
            if self.env < 0.0008 {
                self.active = false;
                return (0.0, 0.0);
            }
        }

        let remaining = limit as f64 - self.pos;
        let end_fade = if remaining < END_FADE_SAMPLES {
            (remaining / END_FADE_SAMPLES).clamp(0.0, 1.0) as f32
        } else {
            1.0
        };

        self.lp_l += self.lp_a * (left - self.lp_l);
        self.lp_r += self.lp_a * (right - self.lp_r);
        if self.lp_l.abs() < 1.0e-20 {
            self.lp_l = 0.0;
        }
        if self.lp_r.abs() < 1.0e-20 {
            self.lp_r = 0.0;
        }

        let level = self.env * end_fade * self.gain * 0.45;
        (self.lp_l * level * self.pan_l, self.lp_r * level * self.pan_r)
    }
}

/// Swing is the fraction of each eighth note occupied by the first sixteenth.
/// 0.5 is straight. Higher values delay the off-sixteenth.
pub fn step_at(beats: f64, swing: f64, steps: usize) -> usize {
    let steps = steps.max(1);
    let pattern_beats = steps as f64 / 4.0;
    let phase = beats.rem_euclid(pattern_beats);
    let eighths = phase * 2.0;
    let pair = eighths.floor() as usize;
    let frac = eighths - pair as f64;
    let swing = swing.clamp(0.5, 0.8);
    let step_in_pair = if frac < swing { 0 } else { 1 };
    (pair * 2 + step_in_pair).min(steps - 1)
}

fn lowpass_coef(tone: f32, sample_rate: f32) -> f32 {
    let tone = tone.clamp(0.0, 1.0);
    if tone >= 0.98 {
        return 1.0;
    }
    let hz = 180.0 * 90.0f32.powf(tone);
    let coefficient = 1.0 - (-2.0 * std::f32::consts::PI * hz / sample_rate.max(1.0)).exp();
    coefficient.clamp(0.0001, 1.0)
}

fn soft_clip(sample: f32) -> f32 {
    (sample * 1.15).tanh()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{LanePattern, Pattern, LANES};
    use crate::sample::SampleData;
    use crate::slice::sensitivity_bucket;

    #[test]
    fn straight_grid_lands_on_sixteenths() {
        assert_eq!(step_at(0.0, 0.5, 16), 0);
        assert_eq!(step_at(0.24, 0.5, 16), 0);
        assert_eq!(step_at(0.25, 0.5, 16), 1);
        assert_eq!(step_at(0.5, 0.5, 16), 2);
        assert_eq!(step_at(1.0, 0.5, 16), 4);
        assert_eq!(step_at(4.0, 0.5, 16), 0);
    }

    #[test]
    fn swing_delays_the_off_sixteenth() {
        assert_eq!(step_at(0.25, 0.66, 16), 0);
        assert_eq!(step_at(0.34, 0.66, 16), 1);
        assert_eq!(step_at(0.5, 0.66, 16), 2);
    }

    #[test]
    fn a_hit_on_beat_two_is_audible_there() {
        let sample_rate = 44_100.0;
        let mut mono = vec![0.0f32; sample_rate as usize];
        for offset in 0..64 {
            let env = (offset as f32 / 8.0).min(1.0) * (1.0 - offset as f32 / 64.0);
            mono[offset] = env;
        }
        let sample = SampleData::from_channels(
            mono.clone(),
            mono,
            sample_rate,
            "click".into(),
            String::new(),
            false,
        );
        let bucket = sensitivity_bucket(0.5);
        assert!(!sample.buckets[bucket].is_empty());

        let mut pattern = Pattern {
            steps: 16,
            bucket,
            lanes: [LanePattern::default(); LANES],
        };
        pattern.lanes[0].slice = 0;
        pattern.lanes[0].velocity[4] = 1.0;
        pattern.lanes[0].gain = 1.0;
        pattern.lanes[0].decay = 0.8;
        pattern.lanes[0].tone = 1.0;

        let mut engine = Engine::new(sample_rate);
        let frames = sample_rate as usize;
        let mut left = vec![0.0f32; frames];
        let mut right = vec![0.0f32; frames];
        let block = 64;
        let mut rendered = 0;
        while rendered < frames {
            let count = block.min(frames - rendered);
            for index in 0..count {
                let beats = (rendered + index) as f64 / f64::from(sample_rate) * 120.0 / 60.0;
                let (l, r) = engine.frame(FrameInput {
                    sample: Some(&sample),
                    pattern: &pattern,
                    playing: true,
                    beats,
                    swing: 0.5,
                    preview_slice: None,
                    gain: 1.0,
                });
                left[rendered + index] = l;
                right[rendered + index] = r;
            }
            rendered += count;
        }

        let around_downbeat = peak_around(&left, 0, 400);
        let beat_two = (0.5 * sample_rate) as usize;
        let around_hit = peak_around(&left, beat_two.saturating_sub(40), beat_two + 800);
        assert!(
            around_hit > 0.05,
            "expected the step-4 hit to sound, peak {around_hit}"
        );
        assert!(
            around_downbeat < around_hit * 0.25,
            "downbeat {around_downbeat} should stay quieter than the hit {around_hit}"
        );
        assert!(left.iter().chain(right.iter()).all(|sample| sample.is_finite()));
    }

    fn peak_around(buffer: &[f32], start: usize, end: usize) -> f32 {
        buffer[start.min(buffer.len())..end.min(buffer.len())]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    }
}
