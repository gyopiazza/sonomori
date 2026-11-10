//! Deterministic beat generator.
//!
//! The same seed and macro settings always rebuild the same pattern. Randomize
//! only changes the seed. Density, syncopation, and symmetry reshape that beat
//! instead of rolling a new one.
//!
//! Lane assignment follows the slice, the way Life groups hits that sound
//! alike. Low prefers a kick-like hit, Mid a snare-like hit, High a hat-like
//! hit. The seed only chooses among slices that already fit that lane.

use crate::sample::SliceInfo;
use crate::slice::MAX_SLICES;

pub const LANES: usize = 4;
pub const STEPS_PER_BAR: usize = 16;
pub const MAX_STEPS: usize = STEPS_PER_BAR * 2;

pub const LANE_LOW: usize = 0;
pub const LANE_MID: usize = 1;
pub const LANE_HIGH: usize = 2;
pub const LANE_EXTRA: usize = 3;

const LANE_GAIN: [f32; LANES] = [1.0, 0.92, 0.52, 0.7];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LanePattern {
    pub velocity: [f32; MAX_STEPS],
    pub slice: usize,
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
    pub bucket: usize,
}

impl Default for Pattern {
    fn default() -> Self {
        Self {
            lanes: [LanePattern::default(); LANES],
            steps: STEPS_PER_BAR,
            bucket: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GenParams {
    pub seed: u32,
    pub density: f32,
    pub syncopation: f32,
    pub symmetry: f32,
    pub sound_var: f32,
    pub pattern_var: f32,
    pub decay: f32,
    pub pitch: f32,
    pub tone: f32,
    pub bars: usize,
    pub bucket: usize,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            seed: 1,
            density: 0.65,
            syncopation: 0.1,
            symmetry: 0.5,
            sound_var: 0.2,
            pattern_var: 0.15,
            decay: 0.55,
            pitch: 0.0,
            tone: 0.75,
            bars: 1,
            bucket: 0,
        }
    }
}

pub fn generate(slices: &[SliceInfo], params: &GenParams) -> Pattern {
    let bars = params.bars.clamp(1, 2);
    let steps = bars * STEPS_PER_BAR;
    let density = params.density.clamp(0.0, 1.0);
    let sync = params.syncopation.clamp(-1.0, 1.0);
    let symmetry = params.symmetry.clamp(0.0, 1.0);
    let pattern_var = params.pattern_var.clamp(0.0, 1.0);
    let sound_var = params.sound_var.clamp(0.0, 1.0);

    let mut pattern = Pattern {
        steps,
        bucket: params.bucket,
        ..Pattern::default()
    };

    let chosen = pick_slices(slices, params.seed);
    for lane in 0..LANES {
        let lane_pattern = &mut pattern.lanes[lane];
        lane_pattern.slice = chosen[lane];
        lane_pattern.gain = LANE_GAIN[lane];
        lane_pattern.pitch = lane_pitch(params.seed, lane, params.pitch, sound_var);
        lane_pattern.decay = lane_decay(params.seed, lane, params.decay, sound_var);
        lane_pattern.tone = lane_tone(params.seed, lane, params.tone, sound_var);
        lane_pattern.pan = lane_pan(params.seed, lane, sound_var);
    }

    for bar in 0..bars {
        for step in 0..8 {
            let index = bar * STEPS_PER_BAR + step;
            write_step(&mut pattern, params.seed, bar, step, index, density, sync, pattern_var);
        }
        for step in 8..STEPS_PER_BAR {
            let index = bar * STEPS_PER_BAR + step;
            let source = bar * STEPS_PER_BAR + (step - 8);
            for lane in 0..LANES {
                let copy = symmetry >= 0.999
                    || hash01(params.seed, 80 + lane as u32, index as u32)
                        < symmetry * (1.0 - pattern_var * 0.35);
                if copy {
                    pattern.lanes[lane].velocity[index] = pattern.lanes[lane].velocity[source];
                } else {
                    pattern.lanes[lane].velocity[index] =
                        hit(params.seed, lane, bar, step, density, sync, pattern_var);
                }
            }
        }
    }

    if bars == 2 {
        for step in 0..STEPS_PER_BAR {
            for lane in 0..LANES {
                let keep = pattern_var <= 0.001
                    || hash01(params.seed, 120 + lane as u32, step as u32)
                        < (1.0 - pattern_var);
                if keep {
                    pattern.lanes[lane].velocity[STEPS_PER_BAR + step] =
                        pattern.lanes[lane].velocity[step];
                }
            }
        }
    }

    pattern
}

fn write_step(
    pattern: &mut Pattern,
    seed: u32,
    bar: usize,
    step: usize,
    index: usize,
    density: f32,
    sync: f32,
    pattern_var: f32,
) {
    for lane in 0..LANES {
        pattern.lanes[lane].velocity[index] =
            hit(seed, lane, bar, step, density, sync, pattern_var);
    }
}

fn hit(
    seed: u32,
    lane: usize,
    bar: usize,
    step: usize,
    density: f32,
    sync: f32,
    pattern_var: f32,
) -> f32 {
    let weight = step_weight(lane, step, density, sync);
    let jitter = (hash01(seed, 200 + lane as u32, (bar * 16 + step) as u32) - 0.5)
        * pattern_var
        * 0.45;
    let probability = (weight + jitter).clamp(0.0, 0.98);
    let roll = hash01(seed, 400 + lane as u32, (bar * 32 + step) as u32);
    if roll >= probability {
        return 0.0;
    }
    let accent = if is_accent(lane, step) { 0.82 } else { 0.48 };
    let variation = hash01(seed, 600 + lane as u32, (bar * 16 + step) as u32);
    (accent + variation * 0.18).clamp(0.2, 1.0)
}

fn is_accent(lane: usize, step: usize) -> bool {
    let pos = step % STEPS_PER_BAR;
    match lane {
        LANE_LOW => pos % 4 == 0,
        LANE_MID => pos == 4 || pos == 12,
        LANE_HIGH => pos % 2 == 0,
        _ => pos % 4 == 2,
    }
}

/// Chance that `step` (0..16 inside a bar) fires on `lane`.
fn step_weight(lane: usize, step: usize, density: f32, sync: f32) -> f32 {
    let pos = step % STEPS_PER_BAR;
    let sync_pos = (sync + 1.0) * 0.5;
    let downbeat = pos % 4 == 0;
    let eighth_off = pos % 4 == 2;
    let sixteenth = pos % 2 == 1;
    let anticipation = matches!(pos, 3 | 7 | 11 | 15);

    match lane {
        LANE_LOW => {
            if pos == 0 {
                0.25 + 0.75 * density
            } else if pos == 8 {
                density * (0.35 + 0.55 * (1.0 - sync_pos * 0.35))
            } else if pos == 4 || pos == 12 {
                density * 0.28 * (1.0 - sync_pos * 0.5)
            } else if eighth_off {
                density * (0.08 + 0.42 * sync_pos)
            } else if anticipation {
                density * 0.38 * sync_pos
            } else {
                density * 0.05 * sync_pos
            }
        }
        LANE_MID => {
            if pos == 4 || pos == 12 {
                0.35 + 0.6 * density
            } else if anticipation {
                density * (0.08 + 0.7 * sync_pos)
            } else if sixteenth {
                density * 0.22 * sync_pos
            } else if downbeat {
                density * 0.08 * (1.0 - sync_pos)
            } else {
                density * 0.06
            }
        }
        LANE_HIGH => {
            let base = 0.08 + density * 0.88;
            if sixteenth {
                base * (0.08 + 0.92 * sync_pos)
            } else if eighth_off {
                base * (0.45 + 0.35 * sync_pos)
            } else {
                base * (0.92 - 0.55 * sync_pos)
            }
        }
        _ => {
            let base = density * 0.42;
            if anticipation {
                base * (0.35 + 0.9 * sync_pos)
            } else if eighth_off {
                base * (0.55 + 0.4 * sync_pos)
            } else if downbeat {
                base * (0.7 - 0.35 * sync_pos)
            } else {
                base * (0.25 + 0.4 * sync_pos)
            }
        }
    }
}

fn pick_slices(slices: &[SliceInfo], seed: u32) -> [usize; LANES] {
    let count = slices.len().min(MAX_SLICES);
    if count == 0 {
        return [0; LANES];
    }

    let mut bassiness_low = f32::MAX;
    let mut bassiness_high = f32::MIN;
    let mut scores = [[0.0f32; 3]; MAX_SLICES];
    for index in 0..count {
        let bassiness = slices[index].low_ratio - slices[index].high_ratio;
        bassiness_low = bassiness_low.min(bassiness);
        bassiness_high = bassiness_high.max(bassiness);
        scores[index] = role_scores(&slices[index]);
    }

    // A shaker or a tone has no kick/snare/hat split. Spread the lanes across
    // the slices instead of forcing a role onto sounds that all match.
    if count == 1 || bassiness_high - bassiness_low < 0.08 {
        return spread_by_index(count, seed);
    }

    let mut used = [false; MAX_SLICES];
    let mut chosen = [0usize; LANES];
    // Kick and hat claim a slice before Mid, so the extremes stay in their lanes.
    chosen[LANE_LOW] = pick_role(&scores, count, 0, &mut used, seed, 900);
    chosen[LANE_HIGH] = pick_role(&scores, count, 2, &mut used, seed, 901);
    chosen[LANE_MID] = pick_role(&scores, count, 1, &mut used, seed, 902);
    chosen[LANE_EXTRA] = pick_role_max(&scores, count, &mut used, seed, 903);
    chosen
}

/// Kick, snare, and hat scores. Absolute, so a file full of hats does not grow
/// a fake kick out of whichever hat is least bright.
fn role_scores(slice: &SliceInfo) -> [f32; 3] {
    let low = slice.low_ratio;
    let mid = slice.mid_ratio;
    let high = slice.high_ratio;
    let punch = (slice.punch / 5.0).clamp(0.2, 1.0);
    let body = length_fit(slice.seconds, 0.08, 0.85);
    let snap = length_fit(slice.seconds, 0.04, 0.45);
    let tick = length_fit(slice.seconds, 0.012, 0.14);

    let kick = low * (0.55 + 0.45 * punch) * (0.4 + 0.6 * body) * (1.0 - high * 0.8);
    // Mid and high together: a snare has a body and noise. A hat is almost all high.
    let snare = (0.65 * mid + 0.35 * high * mid)
        * (0.5 + 0.5 * punch)
        * (0.45 + 0.55 * snap)
        * (1.0 - low * 0.65);
    let hat = (0.6 * high + 0.4 * slice.brightness)
        * (0.35 + 0.65 * tick)
        * (1.0 - low * 0.85)
        * (1.0 - mid * 0.25);
    [kick.max(0.0), snare.max(0.0), hat.max(0.0)]
}

/// 1 inside `[lo, hi]`, falling off outside without ever reaching zero.
fn length_fit(seconds: f32, lo: f32, hi: f32) -> f32 {
    if seconds >= lo && seconds <= hi {
        1.0
    } else if seconds < lo {
        (seconds / lo).clamp(0.25, 1.0)
    } else {
        (hi / seconds).clamp(0.25, 1.0)
    }
}

fn pick_role(
    scores: &[[f32; 3]; MAX_SLICES],
    count: usize,
    role: usize,
    used: &mut [bool; MAX_SLICES],
    seed: u32,
    salt: u32,
) -> usize {
    let pick = pick_top(count, used, seed, salt, |index| scores[index][role]);
    if count > 1 {
        used[pick] = true;
    }
    pick
}

fn pick_role_max(
    scores: &[[f32; 3]; MAX_SLICES],
    count: usize,
    used: &mut [bool; MAX_SLICES],
    seed: u32,
    salt: u32,
) -> usize {
    let pick = pick_top(count, used, seed, salt, |index| {
        scores[index][0].max(scores[index][1]).max(scores[index][2])
    });
    if count > 1 {
        used[pick] = true;
    }
    pick
}

/// Seeded pick among the best unused slices for one score.
///
/// Candidates have to stay close to the winner, so Randomize can swap two
/// kicks and cannot put a hat on the low lane.
fn pick_top(
    count: usize,
    used: &[bool; MAX_SLICES],
    seed: u32,
    salt: u32,
    score_of: impl Fn(usize) -> f32,
) -> usize {
    let mut ranked = [0usize; MAX_SLICES];
    for index in 0..count {
        ranked[index] = index;
    }
    for index in 1..count {
        let mut cursor = index;
        while cursor > 0 {
            let current = ranked[cursor];
            let previous = ranked[cursor - 1];
            let current_score = score_of(current);
            let previous_score = score_of(previous);
            let higher = current_score > previous_score
                || (current_score == previous_score && current < previous);
            if !higher {
                break;
            }
            ranked.swap(cursor, cursor - 1);
            cursor -= 1;
        }
    }

    let best = score_of(ranked[0]);
    let cutoff = best * 0.72;
    let mut candidates = [0usize; 4];
    let mut n = 0usize;
    for slot in 0..count {
        let index = ranked[slot];
        if used[index] {
            continue;
        }
        if n > 0 && score_of(index) < cutoff {
            break;
        }
        candidates[n] = index;
        n += 1;
        if n == 4 {
            break;
        }
    }
    if n == 0 {
        return ranked[0];
    }
    let choice = (hash01(seed, salt, 0) * n as f32) as usize;
    candidates[choice.min(n - 1)]
}

fn spread_by_index(count: usize, seed: u32) -> [usize; LANES] {
    let mut chosen = [0usize; LANES];
    let mut used = [false; MAX_SLICES];
    for lane in 0..LANES {
        let start = (hash01(seed, 900, lane as u32) * count as f32) as usize % count;
        let mut pick = start;
        for offset in 0..count {
            let candidate = (start + offset) % count;
            if !used[candidate] {
                pick = candidate;
                break;
            }
        }
        if count > 1 {
            used[pick] = true;
        }
        chosen[lane] = pick;
    }
    chosen
}

fn lane_pitch(seed: u32, lane: usize, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1000, lane as u32) - 0.5) * 14.0 * sound_var;
    let hat_lift = if lane == LANE_HIGH {
        sound_var * 4.0
    } else {
        0.0
    };
    (global + spread + hat_lift).clamp(-24.0, 24.0)
}

fn lane_decay(seed: u32, lane: usize, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1100, lane as u32) - 0.5) * sound_var;
    let role = match lane {
        LANE_HIGH => 0.72,
        LANE_LOW => 1.05,
        _ => 0.9,
    };
    (global * role + spread).clamp(0.05, 1.0)
}

fn lane_tone(seed: u32, lane: usize, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1200, lane as u32) - 0.5) * sound_var * 0.7;
    let role = match lane {
        LANE_LOW => -0.12,
        LANE_HIGH => 0.18,
        _ => 0.0,
    };
    (global + role * sound_var + spread).clamp(0.0, 1.0)
}

fn lane_pan(seed: u32, lane: usize, sound_var: f32) -> f32 {
    let sign = match lane {
        LANE_HIGH => 1.0,
        LANE_EXTRA => -1.0,
        _ => 0.0,
    };
    let wobble = (hash01(seed, 1300, lane as u32) - 0.5) * 0.35;
    (sign * (0.15 + wobble) * sound_var).clamp(-0.8, 0.8)
}

fn hash01(seed: u32, a: u32, b: u32) -> f32 {
    let mut value = seed
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(a.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(b.wrapping_mul(0xC2B2_AE35));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    (value as f32) / (u32::MAX as f32)
}

#[cfg(test)]
fn active_hits(pattern: &Pattern) -> usize {
    let mut count = 0;
    for lane in &pattern.lanes {
        for step in 0..pattern.steps {
            if lane.velocity[step] > 0.0 {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_slices(count: usize) -> Vec<SliceInfo> {
        (0..count)
            .map(|index| {
                let brightness = index as f32 / count.max(1) as f32;
                SliceInfo {
                    start: index * 1000,
                    end: index * 1000 + 800 + index * 10,
                    peak: 0.8,
                    rms: 0.2,
                    brightness,
                    punch: 3.0,
                    norm_gain: 1.0,
                    low_ratio: 1.0 - brightness,
                    mid_ratio: 0.35,
                    high_ratio: brightness,
                    seconds: 0.12,
                }
            })
            .collect()
    }

    fn role_slice(
        index: usize,
        low: f32,
        mid: f32,
        high: f32,
        seconds: f32,
        brightness: f32,
    ) -> SliceInfo {
        SliceInfo {
            start: index * 2000,
            end: index * 2000 + 1000,
            peak: 0.8,
            rms: 0.2,
            brightness,
            punch: 4.0,
            norm_gain: 1.0,
            low_ratio: low,
            mid_ratio: mid,
            high_ratio: high,
            seconds,
        }
    }

    #[test]
    fn lanes_follow_kick_snare_and_hat() {
        let slices = vec![
            role_slice(0, 0.86, 0.10, 0.04, 0.30, 0.08),
            role_slice(1, 0.80, 0.14, 0.06, 0.28, 0.10),
            role_slice(2, 0.12, 0.55, 0.33, 0.16, 0.45),
            role_slice(3, 0.08, 0.50, 0.42, 0.14, 0.50),
            role_slice(4, 0.04, 0.16, 0.80, 0.04, 0.85),
            role_slice(5, 0.05, 0.20, 0.75, 0.05, 0.80),
            role_slice(6, 0.22, 0.48, 0.30, 0.20, 0.40),
        ];
        let mut seen_low = [false; 2];
        for seed in [1, 7, 40, 200, 999] {
            let pattern = generate(
                &slices,
                &GenParams {
                    seed,
                    ..GenParams::default()
                },
            );
            let low = pattern.lanes[LANE_LOW].slice;
            let mid = pattern.lanes[LANE_MID].slice;
            let high = pattern.lanes[LANE_HIGH].slice;
            let extra = pattern.lanes[LANE_EXTRA].slice;
            assert!(low <= 1, "seed {seed} put slice {low} on Low");
            assert!(
                high == 4 || high == 5,
                "seed {seed} put slice {high} on High"
            );
            assert!(
                mid == 2 || mid == 3 || mid == 6,
                "seed {seed} put slice {mid} on Mid"
            );
            let mut ids = [low, mid, high, extra];
            ids.sort_unstable();
            assert!(
                ids.windows(2).all(|pair| pair[0] != pair[1]),
                "seed {seed} reused a slice: {ids:?}"
            );
            seen_low[low] = true;
        }
        assert!(
            seen_low[0] && seen_low[1],
            "seed should be able to swap the two kicks, saw {seen_low:?}"
        );
    }

    #[test]
    fn same_seed_rebuilds_the_same_pattern() {
        let slices = dummy_slices(12);
        let params = GenParams::default();
        let a = generate(&slices, &params);
        let b = generate(&slices, &params);
        assert_eq!(a, b);
    }

    #[test]
    fn randomize_changes_the_pattern() {
        let slices = dummy_slices(12);
        let mut params = GenParams {
            density: 0.7,
            pattern_var: 0.4,
            ..GenParams::default()
        };
        let first = generate(&slices, &params);
        params.seed = 99;
        let second = generate(&slices, &params);
        assert_ne!(first, second);
    }

    #[test]
    fn full_symmetry_mirrors_each_bar() {
        let slices = dummy_slices(8);
        let params = GenParams {
            symmetry: 1.0,
            pattern_var: 0.0,
            density: 0.8,
            bars: 1,
            ..GenParams::default()
        };
        let pattern = generate(&slices, &params);
        for lane in 0..LANES {
            for step in 0..8 {
                assert_eq!(
                    pattern.lanes[lane].velocity[step],
                    pattern.lanes[lane].velocity[step + 8],
                    "lane {lane} step {step}"
                );
            }
        }
    }

    #[test]
    fn density_increases_the_number_of_hits() {
        let slices = dummy_slices(10);
        let mut sparse = 0;
        let mut dense = 0;
        for seed in 1..16 {
            let low = generate(
                &slices,
                &GenParams {
                    seed,
                    density: 0.05,
                    syncopation: 0.0,
                    symmetry: 0.0,
                    pattern_var: 0.2,
                    ..GenParams::default()
                },
            );
            let high = generate(
                &slices,
                &GenParams {
                    seed,
                    density: 0.95,
                    syncopation: 0.0,
                    symmetry: 0.0,
                    pattern_var: 0.2,
                    ..GenParams::default()
                },
            );
            sparse += active_hits(&low);
            dense += active_hits(&high);
        }
        assert!(dense > sparse, "dense {dense} sparse {sparse}");
    }

    #[test]
    fn zero_pattern_variation_repeats_the_second_bar() {
        let slices = dummy_slices(6);
        let pattern = generate(
            &slices,
            &GenParams {
                bars: 2,
                pattern_var: 0.0,
                symmetry: 0.35,
                density: 0.7,
                ..GenParams::default()
            },
        );
        assert_eq!(pattern.steps, 32);
        for lane in 0..LANES {
            for step in 0..STEPS_PER_BAR {
                assert_eq!(
                    pattern.lanes[lane].velocity[step],
                    pattern.lanes[lane].velocity[STEPS_PER_BAR + step]
                );
            }
        }
    }

    #[test]
    fn positive_syncopation_favors_offbeats() {
        let slices = dummy_slices(10);
        let mut straight = 0;
        let mut pushed = 0;
        for seed in 1..24 {
            for (sync, counter) in [(-1.0, &mut straight), (1.0, &mut pushed)] {
                let pattern = generate(
                    &slices,
                    &GenParams {
                        seed,
                        density: 0.75,
                        syncopation: sync,
                        symmetry: 0.0,
                        pattern_var: 0.15,
                        ..GenParams::default()
                    },
                );
                for lane in &pattern.lanes {
                    for step in 0..pattern.steps {
                        if step % 2 == 1 && lane.velocity[step] > 0.0 {
                            *counter += 1;
                        }
                    }
                }
            }
        }
        assert!(
            pushed > straight,
            "pushed {pushed} straight {straight}"
        );
    }
}
