//! Deterministic beat generator.
//!
//! The same seed and macro settings always rebuild the same pattern. Randomize
//! only changes the seed. Density, syncopation, and symmetry reshape that beat
//! instead of rolling a new one.
//!
//! Lane assignment follows the slice, the way Life groups hits that sound
//! alike. Low prefers a kick-like hit, Mid a snare-like hit, High a hat-like
//! hit. The seed only chooses among slices that already fit that lane.

use crate::features::{self, MAX_SLICES, Role, RoleScores, SliceInfo};
use crate::score::{
    BARS_MAX, BARS_MIN, BEATS_PER_BAR, HALF_BAR, LANES, Lane, Pattern, STEPS_PER_BAR,
    STEPS_PER_BEAT,
};

fn lane_gain(lane: Lane) -> f32 {
    match lane {
        Lane::Low => 1.0,
        Lane::Mid => 0.92,
        Lane::High => 0.52,
        Lane::Extra => 0.7,
    }
}

/// Lane order is the assignment order: earlier lanes keep the slice they claim.
const LANE_ROLES: [(Lane, Role, u32); 3] = [
    (Lane::Low, Role::Kick, 900),
    (Lane::High, Role::Hat, 901),
    (Lane::Mid, Role::Snare, 902),
];

#[derive(Clone, Copy, Debug, PartialEq)]
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
}

/// Collapse knob noise so the cache misses only when a macro actually moves.
/// Every field is named so a new float cannot skip the quantum.
pub fn cache_key(params: &GenParams) -> GenParams {
    GenParams {
        seed: params.seed,
        density: quantize_unit(params.density),
        syncopation: quantize_unit(params.syncopation),
        symmetry: quantize_unit(params.symmetry),
        sound_var: quantize_unit(params.sound_var),
        pattern_var: quantize_unit(params.pattern_var),
        decay: quantize_unit(params.decay),
        pitch: quantize_unit(params.pitch),
        tone: quantize_unit(params.tone),
        bars: params.bars,
    }
}

fn quantize_unit(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            seed: 1,
            density: 0.62,
            syncopation: 0.1,
            symmetry: 0.55,
            sound_var: 0.25,
            pattern_var: 0.2,
            decay: 0.55,
            pitch: 0.0,
            tone: 0.78,
            bars: 1,
        }
    }
}

pub fn generate(slices: &[SliceInfo], params: &GenParams) -> Pattern {
    let bars = params.bars.clamp(BARS_MIN, BARS_MAX);
    let steps = bars * STEPS_PER_BAR;
    let density = params.density.clamp(0.0, 1.0);
    let sync = params.syncopation.clamp(-1.0, 1.0);
    let symmetry = params.symmetry.clamp(0.0, 1.0);
    let pattern_var = params.pattern_var.clamp(0.0, 1.0);
    let sound_var = params.sound_var.clamp(0.0, 1.0);

    let mut pattern = Pattern {
        steps,
        ..Pattern::default()
    };

    let chosen = pick_slices(slices, params.seed);
    for lane in Lane::ALL {
        let index = lane.index();
        let lane_pattern = &mut pattern.lanes[index];
        let slice = slices.get(chosen[index]);
        lane_pattern.slice = chosen[index];
        lane_pattern.start = slice.map(|slice| slice.start).unwrap_or(0);
        lane_pattern.end = slice.map(|slice| slice.end).unwrap_or(0);
        lane_pattern.norm_gain = slice.map(|slice| slice.norm_gain).unwrap_or(1.0);
        lane_pattern.gain = lane_gain(lane);
        lane_pattern.pitch = lane_pitch(params.seed, lane, params.pitch, sound_var);
        lane_pattern.decay = lane_decay(params.seed, lane, params.decay, sound_var);
        lane_pattern.tone = lane_tone(params.seed, lane, params.tone, sound_var);
        lane_pattern.pan = lane_pan(params.seed, lane, sound_var);
    }

    for bar in 0..bars {
        for step in 0..HALF_BAR {
            let index = bar * STEPS_PER_BAR + step;
            write_step(
                &mut pattern,
                params.seed,
                bar,
                step,
                index,
                density,
                sync,
                pattern_var,
            );
        }
        for step in HALF_BAR..STEPS_PER_BAR {
            let index = bar * STEPS_PER_BAR + step;
            let source = bar * STEPS_PER_BAR + (step - HALF_BAR);
            for lane in Lane::ALL {
                let lane_index = lane.index();
                let copy = symmetry >= 0.999
                    || hash01(params.seed, 80 + lane_index as u32, index as u32)
                        < symmetry * (1.0 - pattern_var * 0.35);
                if copy {
                    pattern.lanes[lane_index].velocity[index] =
                        pattern.lanes[lane_index].velocity[source];
                } else {
                    pattern.lanes[lane_index].velocity[index] =
                        hit(params.seed, lane, bar, step, density, sync, pattern_var);
                }
            }
        }
    }

    for bar in 1..bars {
        for step in 0..STEPS_PER_BAR {
            for lane in Lane::ALL {
                let lane_index = lane.index();
                let keep = pattern_var <= 0.001
                    || hash01(params.seed, 120 + lane_index as u32, step as u32)
                        < (1.0 - pattern_var);
                if keep {
                    pattern.lanes[lane_index].velocity[bar * STEPS_PER_BAR + step] =
                        pattern.lanes[lane_index].velocity[step];
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
    for lane in Lane::ALL {
        pattern.lanes[lane.index()].velocity[index] =
            hit(seed, lane, bar, step, density, sync, pattern_var);
    }
}

fn hit(
    seed: u32,
    lane: Lane,
    bar: usize,
    step: usize,
    density: f32,
    sync: f32,
    pattern_var: f32,
) -> f32 {
    let lane_index = lane.index() as u32;
    let weight = step_weight(lane, step, density, sync);
    let jitter = (hash01(seed, 200 + lane_index, (bar * STEPS_PER_BAR + step) as u32) - 0.5)
        * pattern_var
        * 0.45;
    let probability = (weight + jitter).clamp(0.0, 0.98);
    let roll = hash01(
        seed,
        400 + lane_index,
        (bar * (STEPS_PER_BAR * 2) + step) as u32,
    );
    if roll >= probability {
        return 0.0;
    }
    let accent = if is_accent(lane, step) { 0.82 } else { 0.48 };
    let variation = hash01(seed, 600 + lane_index, (bar * STEPS_PER_BAR + step) as u32);
    (accent + variation * 0.18).clamp(0.2, 1.0)
}

struct Grid {
    pos: usize,
    downbeat: bool,
    eighth_off: bool,
    sixteenth: bool,
    anticipation: bool,
    backbeat: bool,
}

fn grid(step: usize) -> Grid {
    let pos = step % STEPS_PER_BAR;
    Grid {
        pos,
        downbeat: pos % STEPS_PER_BEAT == 0,
        eighth_off: pos % STEPS_PER_BEAT == STEPS_PER_BEAT / 2,
        sixteenth: pos % (STEPS_PER_BEAT / 2) == 1,
        anticipation: pos % STEPS_PER_BEAT == STEPS_PER_BEAT - 1,
        backbeat: pos == STEPS_PER_BEAT || pos == STEPS_PER_BEAT * (BEATS_PER_BAR - 1),
    }
}

fn is_accent(lane: Lane, step: usize) -> bool {
    let grid = grid(step);
    match lane {
        Lane::Low => grid.downbeat,
        Lane::Mid => grid.backbeat,
        Lane::High => grid.downbeat || grid.eighth_off,
        Lane::Extra => grid.eighth_off,
    }
}

fn step_weight(lane: Lane, step: usize, density: f32, sync: f32) -> f32 {
    let grid = grid(step);
    let pos = grid.pos;
    let sync_pos = (sync + 1.0) * 0.5;

    match lane {
        Lane::Low => {
            if pos == 0 {
                0.25 + 0.75 * density
            } else if pos == HALF_BAR {
                density * (0.35 + 0.55 * (1.0 - sync_pos * 0.35))
            } else if grid.backbeat {
                density * 0.28 * (1.0 - sync_pos * 0.5)
            } else if grid.eighth_off {
                density * (0.08 + 0.42 * sync_pos)
            } else if grid.anticipation {
                density * 0.38 * sync_pos
            } else {
                density * 0.05 * sync_pos
            }
        }
        Lane::Mid => {
            if grid.backbeat {
                0.35 + 0.6 * density
            } else if grid.anticipation {
                density * (0.08 + 0.7 * sync_pos)
            } else if grid.sixteenth {
                density * 0.22 * sync_pos
            } else if grid.downbeat {
                density * 0.08 * (1.0 - sync_pos)
            } else {
                density * 0.06
            }
        }
        Lane::High => {
            let base = 0.08 + density * 0.88;
            if grid.sixteenth {
                base * (0.08 + 0.92 * sync_pos)
            } else if grid.eighth_off {
                base * (0.45 + 0.35 * sync_pos)
            } else {
                base * (0.92 - 0.55 * sync_pos)
            }
        }
        Lane::Extra => {
            let base = density * 0.42;
            if grid.anticipation {
                base * (0.35 + 0.9 * sync_pos)
            } else if grid.eighth_off {
                base * (0.55 + 0.4 * sync_pos)
            } else if grid.downbeat {
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

    let mut scores = [RoleScores::ZERO; MAX_SLICES];
    for index in 0..count {
        scores[index] = features::role_scores(&slices[index]);
    }

    // A shaker or a tone has no kick/snare/hat split. Spread the lanes across
    // the slices instead of forcing a role onto sounds that all match.
    if !features::distinct_roles(&slices[..count]) {
        return spread_by_index(count, seed);
    }

    let mut used = [false; MAX_SLICES];
    let mut chosen = [0usize; LANES];
    for (lane, role, salt) in LANE_ROLES {
        chosen[lane.index()] = claim(count, &mut used, seed, salt, |index| scores[index].of(role));
    }
    chosen[Lane::Extra.index()] = claim(count, &mut used, seed, 903, |index| scores[index].best());
    chosen
}

fn claim(
    count: usize,
    used: &mut [bool; MAX_SLICES],
    seed: u32,
    salt: u32,
    score_of: impl Fn(usize) -> f32,
) -> usize {
    let pick = pick_top(count, used, seed, salt, score_of);
    used[pick] = true;
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
        if n == candidates.len() {
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

fn lane_pitch(seed: u32, lane: Lane, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1000, lane.index() as u32) - 0.5) * 14.0 * sound_var;
    let hat_lift = if lane == Lane::High {
        sound_var * 4.0
    } else {
        0.0
    };
    (global + spread + hat_lift).clamp(-24.0, 24.0)
}

fn lane_decay(seed: u32, lane: Lane, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1100, lane.index() as u32) - 0.5) * sound_var;
    let role = match lane {
        Lane::High => 0.72,
        Lane::Low => 1.05,
        Lane::Mid | Lane::Extra => 0.9,
    };
    (global * role + spread).clamp(0.05, 1.0)
}

fn lane_tone(seed: u32, lane: Lane, global: f32, sound_var: f32) -> f32 {
    let spread = (hash01(seed, 1200, lane.index() as u32) - 0.5) * sound_var * 0.7;
    let role = match lane {
        Lane::Low => -0.12,
        Lane::High => 0.18,
        Lane::Mid | Lane::Extra => 0.0,
    };
    (global + role * sound_var + spread).clamp(0.0, 1.0)
}

fn lane_pan(seed: u32, lane: Lane, sound_var: f32) -> f32 {
    let sign = match lane {
        Lane::High => 1.0,
        Lane::Extra => -1.0,
        Lane::Low | Lane::Mid => 0.0,
    };
    let wobble = (hash01(seed, 1300, lane.index() as u32) - 0.5) * 0.35;
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
    use crate::score::{LANE_EXTRA, LANE_HIGH, LANE_LOW, LANE_MID};

    fn dummy_slices(count: usize) -> Vec<SliceInfo> {
        (0..count)
            .map(|index| {
                let brightness = index as f32 / count.max(1) as f32;
                SliceInfo {
                    start: index * 1000,
                    end: index * 1000 + 800 + index * 10,
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
        assert!(pushed > straight, "pushed {pushed} straight {straight}");
    }
}
