use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::features;
use crate::score::MIN_SLICE_SAMPLES;
use crate::slice::{self, SENSITIVITY_BUCKETS};

pub const MAX_SECONDS: f32 = 30.0;
pub const IMPORT_EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "ogg", "aac", "m4a"];
const WAVEFORM_COLUMNS: usize = 512;

pub fn formats_phrase() -> String {
    let names: Vec<String> = IMPORT_EXTENSIONS
        .iter()
        .map(|extension| extension.to_uppercase())
        .collect();
    match names.as_slice() {
        [only] => only.clone(),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
        [] => String::new(),
    }
}

pub fn idle_hint() -> String {
    format!(
        "Import a {} file. Longer files use the first {MAX_SECONDS:.0} seconds.",
        formats_phrase()
    )
}

pub fn trimmed_note(trimmed: bool) -> String {
    if trimmed {
        format!("  ·  using the first {MAX_SECONDS:.0} seconds")
    } else {
        String::new()
    }
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub struct WaveformPeak {
    pub min: f32,
    pub max: f32,
}

#[derive(Clone, Debug)]
pub struct SampleData {
    pub name: String,
    pub path: String,
    pub sample_rate: f32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub peaks: Vec<WaveformPeak>,
    pub display_peak: f32,
    /// `buckets[sensitivity_bucket]` is the slice set for that knob position.
    pub buckets: Vec<Vec<features::SliceInfo>>,
    pub generation: u64,
    pub trimmed: bool,
    pub seconds: f32,
}

pub fn load(path: &Path) -> Result<SampleData, String> {
    let file = File::open(path).map_err(|err| format!("Could not open file: {err}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|err| format!("Could not read this audio file ({err})"))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No audio stream in this file".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|err| format!("Could not decode this audio file ({err})"))?;

    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0) as f32;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut trimmed = false;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => break,
        };

        let spec = *decoded.spec();
        if sample_rate <= 0.0 {
            sample_rate = spec.rate as f32;
        }
        if sample_rate <= 0.0 {
            return Err("This file has no sample rate".into());
        }
        let max_frames = (MAX_SECONDS * sample_rate) as usize;
        if left.len() >= max_frames {
            trimmed = true;
            break;
        }

        let channels = spec.channels.count().max(1);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        for frame in buffer.samples().chunks(channels) {
            if left.len() >= max_frames {
                trimmed = true;
                break;
            }
            let first = frame.first().copied().unwrap_or(0.0);
            let second = frame.get(1).copied().unwrap_or(first);
            left.push(first);
            right.push(second);
        }
        if trimmed {
            break;
        }
    }

    if left.is_empty() || sample_rate <= 0.0 {
        return Err("This file did not contain any audio".into());
    }

    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported sound".to_string());

    Ok(SampleData::from_channels(
        left,
        right,
        sample_rate,
        name,
        path.display().to_string(),
        trimmed,
    ))
}

impl SampleData {
    pub fn from_channels(
        left: Vec<f32>,
        right: Vec<f32>,
        sample_rate: f32,
        name: String,
        path: String,
        trimmed: bool,
    ) -> Self {
        let frames = left.len().min(right.len());
        let left = left[..frames].to_vec();
        let right = right[..frames].to_vec();
        let seconds = frames as f32 / sample_rate.max(1.0);

        let mut mono = Vec::with_capacity(frames);
        for index in 0..frames {
            mono.push(0.5 * (left[index] + right[index]));
        }

        let analysis = slice::analyze(&mono, sample_rate);
        let mut buckets = Vec::with_capacity(SENSITIVITY_BUCKETS);
        for bucket in 0..SENSITIVITY_BUCKETS {
            let sensitivity = slice::bucket_sensitivity(bucket);
            let spans = slice::slices_for_sensitivity(&analysis, frames, sample_rate, sensitivity);
            let infos = spans
                .into_iter()
                .map(|(start, end)| features::measure(&left, &right, start, end, sample_rate))
                .filter(|info| info.end.saturating_sub(info.start) >= MIN_SLICE_SAMPLES)
                .collect();
            buckets.push(infos);
        }

        let peaks = waveform_peaks(&left, &right);
        let display_peak = peaks
            .iter()
            .fold(0.0f32, |peak, column| {
                peak.max(column.max.abs()).max(column.min.abs())
            })
            .max(0.001);

        Self {
            name,
            path,
            sample_rate,
            left,
            right,
            peaks,
            display_peak,
            buckets,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            trimmed,
            seconds,
        }
    }

    pub fn len(&self) -> usize {
        self.left.len()
    }
}

fn waveform_peaks(left: &[f32], right: &[f32]) -> Vec<WaveformPeak> {
    let mut peaks = vec![WaveformPeak { min: 0.0, max: 0.0 }; WAVEFORM_COLUMNS];
    let frames = left.len().min(right.len());
    if frames == 0 {
        return peaks;
    }
    for index in 0..frames {
        let column = index * WAVEFORM_COLUMNS / frames;
        let column = column.min(WAVEFORM_COLUMNS - 1);
        let low = left[index].min(right[index]);
        let high = left[index].max(right[index]);
        peaks[column].min = peaks[column].min.min(low);
        peaks[column].max = peaks[column].max.max(high);
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pcm16_mono(path: &Path, sample_rate: u32, samples: &[f32]) {
        let data_bytes = samples.len() * 2;
        let mut bytes = Vec::with_capacity(44 + data_bytes);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        for sample in samples {
            let quantized = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            bytes.extend_from_slice(&quantized.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn decodes_a_wav_and_chops_clicks() {
        let sample_rate = 22_050u32;
        let mut samples = vec![0.0f32; sample_rate as usize];
        for time in [100, 8_000, 14_000] {
            for offset in 0..40 {
                let env = (offset as f32 / 5.0).min(1.0) * (1.0 - offset as f32 / 40.0);
                samples[time + offset] = env;
            }
        }
        let path = std::env::temp_dir().join("sonomori-decode-test.wav");
        write_pcm16_mono(&path, sample_rate, &samples);
        let loaded = load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!((loaded.sample_rate - sample_rate as f32).abs() < 1.0);
        assert_eq!(loaded.buckets.len(), SENSITIVITY_BUCKETS);
        let slices = &loaded.buckets[slice::sensitivity_bucket(0.6)];
        assert!(slices.len() >= 3, "got {} slices", slices.len());
        assert!(!loaded.trimmed);
    }

    #[test]
    fn low_tone_and_noise_land_in_different_bands() {
        let sample_rate = 44_100.0;
        let frames = sample_rate as usize;
        let low: Vec<f32> = (0..frames)
            .map(|index| (index as f32 * 80.0 * std::f32::consts::TAU / sample_rate).sin() * 0.5)
            .collect();
        let low_sample = SampleData::from_channels(
            low.clone(),
            low,
            sample_rate,
            "low".into(),
            String::new(),
            false,
        );
        for info in &low_sample.buckets[0] {
            assert!(
                info.low_ratio > 0.6 && info.low_ratio > info.high_ratio,
                "low {} high {}",
                info.low_ratio,
                info.high_ratio
            );
        }

        let mut state = 0x1234_5678u32;
        let noise: Vec<f32> = (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 16) as f32 / 32_768.0 - 1.0) * 0.3
            })
            .collect();
        let noise_sample = SampleData::from_channels(
            noise.clone(),
            noise,
            sample_rate,
            "noise".into(),
            String::new(),
            false,
        );
        for info in &noise_sample.buckets[0] {
            assert!(
                info.high_ratio > info.low_ratio,
                "low {} high {}",
                info.low_ratio,
                info.high_ratio
            );
        }
    }
}
