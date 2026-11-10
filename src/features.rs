//! Measurements that decide which lane a slice belongs to.
//!
//! The loader computes them once. The pattern generator is the only reader.
//! Band edges and role scores stay together because a kick is both.

/// Hard cap so the pattern generator and the UI stay bounded.
pub const MAX_SLICES: usize = 48;

#[derive(Clone, Copy, Debug)]
pub enum Role {
    Kick,
    Snare,
    Hat,
}

#[derive(Clone, Copy, Debug)]
pub struct RoleScores {
    kick: f32,
    snare: f32,
    hat: f32,
}

impl RoleScores {
    pub const ZERO: Self = Self {
        kick: 0.0,
        snare: 0.0,
        hat: 0.0,
    };

    pub fn of(self, role: Role) -> f32 {
        match role {
            Role::Kick => self.kick,
            Role::Snare => self.snare,
            Role::Hat => self.hat,
        }
    }

    pub fn best(self) -> f32 {
        self.of(Role::Kick)
            .max(self.of(Role::Snare))
            .max(self.of(Role::Hat))
    }
}

/// What the generator knows about one slice. Playback only sees the span copied onto the lane.
#[derive(Clone, Debug)]
pub struct SliceInfo {
    pub start: usize,
    pub end: usize,
    pub brightness: f32,
    pub punch: f32,
    pub norm_gain: f32,
    /// Share of energy below ~180 Hz. Kicks live here.
    pub low_ratio: f32,
    /// Share of energy between ~180 Hz and ~4.5 kHz.
    pub mid_ratio: f32,
    /// Share of energy above ~4.5 kHz. Hats live here.
    pub high_ratio: f32,
    pub seconds: f32,
}

pub fn measure(
    left: &[f32],
    right: &[f32],
    start: usize,
    end: usize,
    sample_rate: f32,
) -> SliceInfo {
    let end = end.min(left.len()).min(right.len());
    let start = start.min(end);
    let rate = sample_rate.max(1.0);
    let low_coef = one_pole_coef(180.0, rate);
    let high_coef = one_pole_coef(4_500.0, rate);

    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    let mut low_energy = 0.0f64;
    let mut mid_energy = 0.0f64;
    let mut high_energy = 0.0f64;
    let mut low = 0.0f32;
    let mut body = 0.0f32;
    let mut crossings = 0usize;
    let mut previous = 0.0f32;
    let span = end.saturating_sub(start).max(1);

    for index in start..end {
        let sample = 0.5 * (left[index] + right[index]);
        low += low_coef * (sample - low);
        body += high_coef * (sample - body);
        let high = sample - body;
        let mid = body - low;
        low_energy += f64::from(low) * f64::from(low);
        mid_energy += f64::from(mid) * f64::from(mid);
        high_energy += f64::from(high) * f64::from(high);

        peak = peak.max(left[index].abs()).max(right[index].abs());
        sum += f64::from(sample) * f64::from(sample);
        if index > start && previous.signum() != sample.signum() && sample.abs() > 1.0e-5 {
            crossings += 1;
        }
        previous = sample;
    }

    let total = low_energy + mid_energy + high_energy;
    let (low_ratio, mid_ratio, high_ratio) = if total > 1.0e-12 {
        (
            (low_energy / total) as f32,
            (mid_energy / total) as f32,
            (high_energy / total) as f32,
        )
    } else {
        (0.0, 0.0, 0.0)
    };
    let rms = (sum / span as f64).sqrt() as f32;
    let zcr = crossings as f32 / span as f32;
    let brightness = (0.65 * high_ratio + 0.35 * (zcr / 0.08).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    let punch = if rms > 1.0e-6 { peak / rms } else { 1.0 };
    let norm_gain = if peak < 1.0e-4 {
        1.0
    } else {
        (0.9 / peak).clamp(0.2, 4.0)
    };
    SliceInfo {
        start,
        end,
        brightness,
        punch,
        norm_gain,
        low_ratio,
        mid_ratio,
        high_ratio,
        seconds: span as f32 / rate,
    }
}

fn one_pole_coef(hz: f32, sample_rate: f32) -> f32 {
    let hz = hz.clamp(1.0, sample_rate * 0.45);
    1.0 - (-std::f32::consts::TAU * hz / sample_rate).exp()
}

pub fn role_scores(slice: &SliceInfo) -> RoleScores {
    let low = slice.low_ratio;
    let mid = slice.mid_ratio;
    let high = slice.high_ratio;
    let punch = (slice.punch / 5.0).clamp(0.2, 1.0);
    let body = length_fit(slice.seconds, 0.08, 0.85);
    let snap = length_fit(slice.seconds, 0.04, 0.45);
    let tick = length_fit(slice.seconds, 0.012, 0.14);

    let kick = low * (0.55 + 0.45 * punch) * (0.4 + 0.6 * body) * (1.0 - high * 0.8);
    let snare = (0.65 * mid + 0.35 * high * mid)
        * (0.5 + 0.5 * punch)
        * (0.45 + 0.55 * snap)
        * (1.0 - low * 0.65);
    let hat = (0.6 * high + 0.4 * slice.brightness)
        * (0.35 + 0.65 * tick)
        * (1.0 - low * 0.85)
        * (1.0 - mid * 0.25);
    RoleScores {
        kick: kick.max(0.0),
        snare: snare.max(0.0),
        hat: hat.max(0.0),
    }
}

fn length_fit(seconds: f32, lo: f32, hi: f32) -> f32 {
    if seconds >= lo && seconds <= hi {
        1.0
    } else if seconds < lo {
        (seconds / lo).clamp(0.25, 1.0)
    } else {
        (hi / seconds).clamp(0.25, 1.0)
    }
}
