//! Energy-flux transient detection.
//!
//! The file is scanned once. Each sensitivity bucket reuses that flux curve and
//! only changes the peak threshold, the minimum gap, and the longest slice.

/// Number of precomputed sensitivity settings, from fewest slices to most.
pub const SENSITIVITY_BUCKETS: usize = 17;
/// Hard cap so the pattern generator and the UI stay bounded.
pub const MAX_SLICES: usize = 48;

/// Half-wave flux of the short-time RMS envelope.
pub struct OnsetAnalysis {
    hop: usize,
    flux: Vec<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Onset {
    sample: usize,
    score: f32,
}

/// Map a 0..1 sensitivity knob onto a precomputed bucket.
pub fn sensitivity_bucket(sensitivity: f32) -> usize {
    let max = (SENSITIVITY_BUCKETS - 1) as f32;
    (sensitivity.clamp(0.0, 1.0) * max).round() as usize
}

/// Sensitivity represented by `bucket` (0 = sparsest, last = busiest).
pub fn bucket_sensitivity(bucket: usize) -> f32 {
    if SENSITIVITY_BUCKETS <= 1 {
        return 0.5;
    }
    bucket.clamp(0, SENSITIVITY_BUCKETS - 1) as f32 / (SENSITIVITY_BUCKETS - 1) as f32
}

pub fn analyze(mono: &[f32], sample_rate: f32) -> OnsetAnalysis {
    let hop = ((sample_rate * 0.003).round() as usize).clamp(32, 512);
    if mono.len() < hop * 4 {
        return OnsetAnalysis {
            hop,
            flux: Vec::new(),
        };
    }

    let frames = mono.len() / hop;
    let mut energy = vec![0.0f32; frames];
    for (index, bin) in energy.iter_mut().enumerate() {
        let start = index * hop;
        let end = (start + hop).min(mono.len());
        let mut sum = 0.0f32;
        for sample in &mono[start..end] {
            sum += sample * sample;
        }
        *bin = (sum / (end - start) as f32).sqrt();
    }

    let mut flux = vec![0.0f32; frames];
    for index in 1..frames {
        flux[index] = (energy[index] - energy[index - 1]).max(0.0);
    }

    OnsetAnalysis { hop, flux }
}

/// Slice boundaries as half-open sample ranges `[start, end)`.
pub fn slices_for_sensitivity(
    analysis: &OnsetAnalysis,
    frame_count: usize,
    sample_rate: f32,
    sensitivity: f32,
) -> Vec<(usize, usize)> {
    if frame_count < 8 {
        return Vec::new();
    }
    if analysis.flux.len() < 4 || analysis.hop == 0 {
        return even_slices(frame_count, 8);
    }

    let sensitivity = sensitivity.clamp(0.0, 1.0);
    let hop = analysis.hop;
    let flux = &analysis.flux;
    let frames = flux.len();

    let mut prefix = vec![0.0f32; frames + 1];
    for (index, value) in flux.iter().enumerate() {
        prefix[index + 1] = prefix[index] + value;
    }

    // Higher sensitivity lowers the threshold and allows closer cuts.
    let multiplier = 3.4 - sensitivity * 2.6;
    let min_gap_samples = ((0.20 - sensitivity * 0.15).max(0.035) * sample_rate) as usize;
    let min_gap_frames = (min_gap_samples / hop).max(1);
    let local = ((0.18 * sample_rate) as usize / hop).clamp(4, 64);
    let floor = 0.004 * (1.15 - sensitivity);

    let mut onsets = vec![Onset {
        sample: 0,
        score: f32::MAX,
    }];
    let mut last_frame = 0isize;

    for index in 2..frames.saturating_sub(1) {
        let is_peak = flux[index] >= flux[index - 1] && flux[index] >= flux[index + 1];
        if !is_peak || index as isize - last_frame < min_gap_frames as isize {
            continue;
        }
        let left = index.saturating_sub(local);
        let right = (index + local).min(frames);
        let mean = (prefix[right] - prefix[left]) / (right - left) as f32;
        let threshold = (mean * multiplier).max(floor);
        if flux[index] > threshold {
            onsets.push(Onset {
                sample: index * hop,
                score: flux[index],
            });
            last_frame = index as isize;
        }
    }

    if onsets.len() > MAX_SLICES {
        let first = onsets[0];
        onsets[1..].sort_by(|a, b| b.score.total_cmp(&a.score));
        onsets.truncate(MAX_SLICES);
        onsets[0] = first;
        onsets.sort_by(|a, b| a.sample.cmp(&b.sample));
    }

    if onsets.len() < 2 {
        return even_slices(frame_count, 8);
    }

    let mut spans = Vec::with_capacity(onsets.len());
    for pair in onsets.windows(2) {
        if pair[1].sample > pair[0].sample {
            spans.push((pair[0].sample, pair[1].sample));
        }
    }
    let last = onsets[onsets.len() - 1].sample;
    if last + hop < frame_count {
        spans.push((last, frame_count));
    }

    let max_len = ((0.40 + (1.0 - sensitivity) * 1.15) * sample_rate) as usize;
    subdivide_long(&mut spans, max_len.max(hop * 2), frame_count);
    merge_until(&mut spans, MAX_SLICES);
    drop_tiny(&mut spans, (0.03 * sample_rate) as usize, frame_count);

    if spans.is_empty() {
        even_slices(frame_count, 8)
    } else {
        spans
    }
}

fn even_slices(frame_count: usize, count: usize) -> Vec<(usize, usize)> {
    let count = count.max(1).min(frame_count.max(1));
    let mut spans = Vec::with_capacity(count);
    for index in 0..count {
        let start = frame_count * index / count;
        let end = frame_count * (index + 1) / count;
        if end > start {
            spans.push((start, end));
        }
    }
    spans
}

fn subdivide_long(spans: &mut Vec<(usize, usize)>, max_len: usize, frame_count: usize) {
    if max_len == 0 {
        return;
    }
    let mut index = 0;
    while index < spans.len() {
        let (start, end) = spans[index];
        let end = end.min(frame_count);
        if end.saturating_sub(start) <= max_len {
            spans[index] = (start, end);
            index += 1;
            continue;
        }
        let parts = end.saturating_sub(start).div_ceil(max_len).max(2);
        let mut replacements = Vec::with_capacity(parts);
        for part in 0..parts {
            let part_start = start + (end - start) * part / parts;
            let part_end = start + (end - start) * (part + 1) / parts;
            if part_end > part_start {
                replacements.push((part_start, part_end));
            }
        }
        spans.splice(index..index + 1, replacements.iter().copied());
        index += replacements.len();
    }
}

fn merge_until(spans: &mut Vec<(usize, usize)>, max_count: usize) {
    while spans.len() > max_count && spans.len() >= 2 {
        let mut shortest = 0;
        let mut shortest_len = usize::MAX;
        for (index, (start, end)) in spans.iter().enumerate() {
            let len = end.saturating_sub(*start);
            if len < shortest_len {
                shortest_len = len;
                shortest = index;
            }
        }
        if shortest + 1 < spans.len() {
            spans[shortest].1 = spans[shortest + 1].1;
            spans.remove(shortest + 1);
        } else {
            let previous = shortest - 1;
            spans[previous].1 = spans[shortest].1;
            spans.remove(shortest);
        }
    }
}

fn drop_tiny(spans: &mut Vec<(usize, usize)>, min_len: usize, frame_count: usize) {
    if spans.len() < 2 || min_len == 0 {
        return;
    }
    let mut index = 0;
    while index < spans.len() && spans.len() >= 2 {
        let len = spans[index].1.saturating_sub(spans[index].0);
        if len >= min_len {
            index += 1;
            continue;
        }
        if index + 1 < spans.len() {
            spans[index + 1].0 = spans[index].0;
            spans.remove(index);
        } else {
            spans[index - 1].1 = spans[index].1.min(frame_count);
            spans.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_train(sample_rate: f32, times_ms: &[f32], level: f32) -> Vec<f32> {
        let frames = (sample_rate * 2.0) as usize;
        let mut mono = vec![0.0f32; frames];
        for time in times_ms {
            let start = (*time / 1000.0 * sample_rate) as usize;
            for offset in 0..48 {
                let index = start + offset;
                if index >= mono.len() {
                    break;
                }
                let env = (offset as f32 / 6.0).min(1.0) * (1.0 - offset as f32 / 48.0);
                mono[index] += level * env;
            }
        }
        mono
    }

    #[test]
    fn strong_clicks_become_separate_slices() {
        let sample_rate = 44_100.0;
        let mono = click_train(sample_rate, &[20.0, 280.0, 540.0, 860.0], 1.0);
        let analysis = analyze(&mono, sample_rate);
        let slices = slices_for_sensitivity(&analysis, mono.len(), sample_rate, 0.55);
        assert!(
            slices.len() >= 4,
            "expected at least 4 slices, got {}",
            slices.len()
        );
        assert_eq!(slices[0].0, 0);
        for pair in slices.windows(2) {
            assert!(pair[1].0 >= pair[0].1 || pair[1].0 > pair[0].0);
            assert!(pair[0].1 > pair[0].0);
        }
        assert_eq!(slices.last().unwrap().1, mono.len());
    }

    #[test]
    fn higher_sensitivity_keeps_more_slices() {
        let sample_rate = 44_100.0;
        let mut times = vec![30.0, 400.0, 820.0, 1300.0];
        // Weaker clicks between the strong ones.
        times.extend([180.0, 620.0, 1080.0]);
        let mut mono = click_train(sample_rate, &[30.0, 400.0, 820.0, 1300.0], 1.0);
        let weak = click_train(sample_rate, &[180.0, 620.0, 1080.0], 0.28);
        for (slot, sample) in mono.iter_mut().zip(weak) {
            *slot += sample;
        }
        let analysis = analyze(&mono, sample_rate);
        let low = slices_for_sensitivity(&analysis, mono.len(), sample_rate, 0.05);
        let high = slices_for_sensitivity(&analysis, mono.len(), sample_rate, 0.95);
        assert!(
            high.len() >= low.len(),
            "high {} should be >= low {}",
            high.len(),
            low.len()
        );
        assert!(high.len() > low.len());
    }

    #[test]
    fn sustained_tone_still_produces_slices() {
        let sample_rate = 44_100.0;
        let frames = sample_rate as usize;
        let mono: Vec<f32> = (0..frames)
            .map(|index| (index as f32 * 0.02).sin() * 0.4)
            .collect();
        let analysis = analyze(&mono, sample_rate);
        let slices = slices_for_sensitivity(&analysis, mono.len(), sample_rate, 0.4);
        assert!(slices.len() >= 2);
        assert_eq!(slices.last().unwrap().1, mono.len());
    }
}
