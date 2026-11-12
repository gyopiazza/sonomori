use crate::features::MIN_SLICE_SAMPLES;
use crate::score::{LANES, Lane, MAX_STEPS, Pattern, step_at};

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
        if output.abs() < 1.0e-20 { 0.0 } else { output }
    }
}

pub struct Engine {
    sample_rate: f32,
    voices: [Voice; LANES],
    /// Click-to-preview ignores the transport fade. The lane voices do not.
    preview: Voice,
    last_step: Option<usize>,
    stop_env: f32,
    dc_l: DcBlock,
    dc_r: DcBlock,
}

#[derive(Clone, Copy)]
pub struct Audio<'a> {
    pub sample_rate: f32,
    pub left: &'a [f32],
    pub right: &'a [f32],
}

#[derive(Clone, Copy)]
pub struct Preview {
    pub start: usize,
    pub end: usize,
    pub norm_gain: f32,
}

pub struct FrameInput<'a> {
    pub audio: Option<Audio<'a>>,
    pub pattern: &'a Pattern,
    pub playing: bool,
    pub beats: f64,
    pub swing: f32,
    pub preview: Option<Preview>,
    pub gain: f32,
}

impl Engine {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1.0),
            voices: [Voice::default(); LANES],
            preview: Voice::default(),
            last_step: None,
            stop_env: 0.0,
            dc_l: DcBlock::default(),
            dc_r: DcBlock::default(),
        }
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn reset(&mut self) {
        self.voices = [Voice::default(); LANES];
        self.preview = Voice::default();
        self.last_step = None;
        self.stop_env = 0.0;
        self.dc_l = DcBlock::default();
        self.dc_r = DcBlock::default();
    }

    pub fn disarm_step(&mut self) {
        self.last_step = None;
    }

    pub fn any_active(&self) -> bool {
        self.voices.iter().any(|voice| voice.active) || self.preview.active || self.stop_env > 0.0
    }

    pub fn frame(&mut self, input: FrameInput<'_>) -> (f32, f32) {
        let fade_step = 1.0 / (0.012 * self.sample_rate).max(1.0);
        if input.playing {
            self.stop_env = (self.stop_env + fade_step).min(1.0);
        } else {
            self.stop_env = (self.stop_env - fade_step).max(0.0);
            if self.stop_env == 0.0 {
                for voice in &mut self.voices {
                    voice.active = false;
                }
                self.last_step = None;
            }
        }

        let steps = input.pattern.steps.clamp(1, MAX_STEPS);
        if input.playing {
            let step = step_at(input.beats, input.swing as f64, steps);
            if self.last_step != Some(step) {
                self.trigger_step(input.audio, input.pattern, step);
                self.last_step = Some(step);
            }
        }

        if let Some(preview) = input.preview {
            self.trigger_preview(input.audio, preview);
        }

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        if let Some(audio) = input.audio {
            for voice in &mut self.voices {
                let (voice_l, voice_r) = voice.render(audio);
                left += voice_l * self.stop_env;
                right += voice_r * self.stop_env;
            }
            let (preview_l, preview_r) = self.preview.render(audio);
            left += preview_l;
            right += preview_r;
        }

        left = soft_clip(self.dc_l.process(left) * input.gain);
        right = soft_clip(self.dc_r.process(right) * input.gain);
        (left, right)
    }

    fn trigger_step(&mut self, audio: Option<Audio<'_>>, pattern: &Pattern, step: usize) {
        let Some(audio) = audio else {
            return;
        };
        for lane in Lane::ALL {
            let played = &pattern.lanes[lane.index()];
            let velocity = played.velocity[step];
            if velocity <= 0.0 {
                continue;
            }
            self.voices[lane.index()].trigger(
                audio,
                played.start,
                played.end,
                played.pitch,
                played.decay,
                played.tone,
                played.gain * played.norm_gain * velocity,
                played.pan,
                self.sample_rate,
            );
        }
    }

    fn trigger_preview(&mut self, audio: Option<Audio<'_>>, preview: Preview) {
        let Some(audio) = audio else {
            return;
        };
        self.preview.trigger(
            audio,
            preview.start,
            preview.end,
            0.0,
            0.7,
            1.0,
            preview.norm_gain,
            0.0,
            self.sample_rate,
        );
    }
}

impl Voice {
    fn trigger(
        &mut self,
        audio: Audio<'_>,
        start: usize,
        end: usize,
        pitch: f32,
        decay: f32,
        tone: f32,
        gain: f32,
        pan: f32,
        host_rate: f32,
    ) {
        let end = end.min(audio.left.len()).min(audio.right.len());
        if end.saturating_sub(start) < MIN_SLICE_SAMPLES
            || host_rate <= 0.0
            || audio.sample_rate <= 0.0
        {
            self.active = false;
            return;
        }
        let ratio = 2.0f64.powf((pitch as f64) / 12.0);
        self.pos = start as f64;
        self.end = end;
        self.increment = (audio.sample_rate as f64 / host_rate as f64) * ratio;
        self.increment = self.increment.clamp(0.05, 16.0);
        self.env = 0.0;
        self.attack_left = ATTACK_SAMPLES;
        let slice_seconds = (end - start) as f32 / audio.sample_rate;
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

    fn render(&mut self, audio: Audio<'_>) -> (f32, f32) {
        if !self.active {
            return (0.0, 0.0);
        }
        let limit = self.end.min(audio.left.len()).min(audio.right.len());
        if self.pos + 1.0 >= limit as f64 {
            self.active = false;
            return (0.0, 0.0);
        }

        let index = self.pos as usize;
        let frac = (self.pos - index as f64) as f32;
        let next = index + 1;
        if next >= audio.left.len() || next >= audio.right.len() {
            self.active = false;
            return (0.0, 0.0);
        }
        let left = audio.left[index] * (1.0 - frac) + audio.left[next] * frac;
        let right = audio.right[index] * (1.0 - frac) + audio.right[next] * frac;
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
        (
            self.lp_l * level * self.pan_l,
            self.lp_r * level * self.pan_r,
        )
    }
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
    use crate::sample::SampleData;
    use crate::score::{LANES, LanePattern, Pattern};
    use crate::slice::sensitivity_bucket;

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
        let slice = &sample.buckets[bucket][0];
        assert!(slice.end > slice.start + 1);

        let mut pattern = Pattern {
            steps: 16,
            lanes: [LanePattern::default(); LANES],
        };
        pattern.lanes[0].slice = 0;
        pattern.lanes[0].start = slice.start;
        pattern.lanes[0].end = slice.end;
        pattern.lanes[0].norm_gain = slice.norm_gain;
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
        let audio = Audio {
            sample_rate,
            left: &sample.left,
            right: &sample.right,
        };
        while rendered < frames {
            let count = block.min(frames - rendered);
            for index in 0..count {
                let beats = crate::session::beats_from_samples(
                    (rendered + index) as f64,
                    f64::from(crate::session::DEFAULT_BPM),
                    f64::from(sample_rate),
                );
                let (l, r) = engine.frame(FrameInput {
                    audio: Some(audio),
                    pattern: &pattern,
                    playing: true,
                    beats,
                    swing: 0.5,
                    preview: None,
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
        assert!(
            left.iter()
                .chain(right.iter())
                .all(|sample| sample.is_finite())
        );
    }

    fn peak_around(buffer: &[f32], start: usize, end: usize) -> f32 {
        buffer[start.min(buffer.len())..end.min(buffer.len())]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    }
}
