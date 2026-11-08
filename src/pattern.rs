//! Deterministic beat generator.
//!
//! The same seed and macro settings always rebuild the same pattern. Randomize
//! only changes the seed. Density, syncopation, and symmetry reshape that beat
//! instead of rolling a new one.

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

    let mut by_brightness = [0usize; MAX_SLICES];
    for index in 0..count {
        by_brightness[index] = index;
    }
    for index in 1..count {
        let mut cursor = index;
        while cursor > 0
            && slices[by_brightness[cursor]].brightness
                < slices[by_brightness[cursor - 1]].brightness
        {
            by_brightness.swap(cursor, cursor - 1);
            cursor -= 1;
        }
    }

    let low_end = (count / 3).clamp(1, count);
    let mid_end = ((count * 2) / 3).clamp(low_end, count);
    let mut pools = [(0usize, count); LANES];
    pools[LANE_LOW] = (0, low_end);
    pools[LANE_MID] = (low_end.saturating_sub((count > 1) as usize), mid_end.max(low_end));
    pools[LANE_HIGH] = (mid_end.saturating_sub((count > 2) as usize), count);
    pools[LANE_EXTRA] = (0, count);
    if pools[LANE_MID].1 <= pools[LANE_MID].0 {
        pools[LANE_MID] = (0, count);
    }
    if pools[LANE_HIGH].1 <= pools[LANE_HIGH].0 {
        pools[LANE_HIGH] = (0, count);
    }

    let mut chosen = [0usize; LANES];
    let mut used = [false; MAX_SLICES];
    for lane in 0..LANES {
        let (start, end) = pools[lane];
        let span = (end - start).max(1);
        let mut pick = start + (hash01(seed, 900, lane as u32) * span as f32) as usize;
        if pick >= end {
            pick = end - 1;
        }
        let mut resolved = by_brightness[pick];
        if lane == LANE_EXTRA {
            resolved = longest_unused(slices, count, &used, seed);
        } else if used[resolved] {
            for offset in 1..count {
                let candidate = by_brightness[(pick + offset) % count];
                if !used[candidate] {
                    resolved = candidate;
                    break;
                }
            }
        }
        if count > 1 {
            used[resolved] = true;
        }
        chosen[lane] = resolved;
    }
    chosen
}

fn longest_unused(slices: &[SliceInfo], count: usize, used: &[bool], seed: u32) -> usize {
    let mut best = 0usize;
    let mut best_len = 0usize;
    let mut found = false;
    for index in 0..count {
        if used[index] {
            continue;
        }
        let len = slices[index].end.saturating_sub(slices[index].start);
        let jitter = (hash01(seed, 910, index as u32) * 64.0) as usize;
        if !found || len + jitter > best_len {
            best = index;
            best_len = len + jitter;
            found = true;
        }
    }
    if found { best } else { 0 }
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
            .map(|index| SliceInfo {
                start: index * 1000,
                end: index * 1000 + 800 + index * 10,
                peak: 0.8,
                rms: 0.2,
                brightness: index as f32 / count.max(1) as f32,
                punch: 2.0,
                norm_gain: 1.0,
            })
            .collect()
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
