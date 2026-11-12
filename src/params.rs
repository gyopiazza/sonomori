//! Host parameters. The pattern generator reads these through [`gen_params`]
//! and does not depend on this module.

use std::sync::Arc;

use nice_plug::prelude::*;
use parking_lot::Mutex;

use crate::pattern::GenParams;
use crate::score::{BARS_MAX, BARS_MIN, SWING_MAX, SWING_MIN};
use crate::session::DEFAULT_BPM;

pub const SEED_MIN: i32 = 1;
pub const SEED_MAX: i32 = 999_999;

#[derive(Params)]
pub struct SonomoriParams {
    #[id = "density"]
    pub density: FloatParam,
    #[id = "syncopation"]
    pub syncopation: FloatParam,
    #[id = "symmetry"]
    pub symmetry: FloatParam,
    #[id = "pattern_var"]
    pub pattern_var: FloatParam,
    #[id = "sound_var"]
    pub sound_var: FloatParam,
    #[id = "swing"]
    pub swing: FloatParam,
    #[id = "decay"]
    pub decay: FloatParam,
    #[id = "pitch"]
    pub pitch: IntParam,
    #[id = "tone"]
    pub tone: FloatParam,
    #[id = "sensitivity"]
    pub sensitivity: FloatParam,
    #[id = "bars"]
    pub bars: IntParam,
    #[id = "tempo"]
    pub tempo: FloatParam,
    #[id = "output"]
    pub output: FloatParam,
    #[id = "seed"]
    pub seed: IntParam,

    /// Restored with the DAW project so the imported file reloads next session.
    #[persist = "sample_path"]
    pub sample_path: Mutex<String>,
}

impl Default for SonomoriParams {
    fn default() -> Self {
        let groove = GenParams::default();
        Self {
            density: percent_param("Density", groove.density),
            syncopation: bipolar_param("Syncopation", groove.syncopation),
            symmetry: percent_param("Symmetry", groove.symmetry),
            pattern_var: percent_param("Pattern Variation", groove.pattern_var),
            sound_var: percent_param("Sound Variation", groove.sound_var),
            swing: ratio_param("Swing", SWING_MIN, SWING_MIN, SWING_MAX),
            decay: percent_param("Decay", groove.decay),
            pitch: IntParam::new(
                "Pitch",
                groove.pitch as i32,
                IntRange::Linear { min: -12, max: 12 },
            )
            .with_unit(" st"),
            tone: percent_param("Tone", groove.tone),
            sensitivity: percent_param("Sensitivity", 0.45),
            bars: IntParam::new(
                "Bars",
                groove.bars as i32,
                IntRange::Linear {
                    min: BARS_MIN as i32,
                    max: BARS_MAX as i32,
                },
            ),
            tempo: FloatParam::new(
                "Audition BPM",
                DEFAULT_BPM,
                FloatRange::Linear {
                    min: 40.0,
                    max: 240.0,
                },
            )
            .with_step_size(1.0)
            .with_unit(" BPM")
            .with_value_to_string(formatters::v2s_f32_rounded(0)),
            output: FloatParam::new(
                "Output",
                util::db_to_gain(0.0),
                FloatRange::gain_range(-24.0, 6.0),
            )
            .with_smoother(SmoothingStyle::Logarithmic(30.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            seed: IntParam::new(
                "Seed",
                groove.seed as i32,
                IntRange::Linear {
                    min: SEED_MIN,
                    max: SEED_MAX,
                },
            ),
            sample_path: Mutex::new(String::new()),
        }
    }
}

impl SonomoriParams {
    /// Bits of every host parameter the picture can show. Adding a parameter
    /// without mixing it here fails `every_host_param_moves_the_picture_key`.
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut hash = mix(0xcbf29ce484222325, 1);
        hash = mix_float(hash, &self.density);
        hash = mix_float(hash, &self.syncopation);
        hash = mix_float(hash, &self.symmetry);
        hash = mix_float(hash, &self.pattern_var);
        hash = mix_float(hash, &self.sound_var);
        hash = mix_float(hash, &self.swing);
        hash = mix_float(hash, &self.decay);
        hash = mix_float(hash, &self.tone);
        hash = mix_float(hash, &self.sensitivity);
        hash = mix_float(hash, &self.tempo);
        hash = mix_float(hash, &self.output);
        hash = mix_int(hash, &self.pitch);
        hash = mix_int(hash, &self.bars);
        mix_int(hash, &self.seed)
    }
}

pub fn gen_params(params: &SonomoriParams) -> GenParams {
    GenParams {
        seed: params.seed.value().max(0) as u32,
        density: params.density.value(),
        syncopation: params.syncopation.value(),
        symmetry: params.symmetry.value(),
        sound_var: params.sound_var.value(),
        pattern_var: params.pattern_var.value(),
        decay: params.decay.value(),
        pitch: params.pitch.value() as f32,
        tone: params.tone.value(),
        bars: (params.bars.value() as usize).clamp(BARS_MIN, BARS_MAX),
    }
}

fn bipolar_param(name: &'static str, default: f32) -> FloatParam {
    FloatParam::new(
        name,
        default,
        FloatRange::Linear {
            min: -1.0,
            max: 1.0,
        },
    )
    .with_step_size(RATIO_STEP)
    .with_value_to_string(Arc::new(|value| format!("{:+.0}", value * 100.0)))
    .with_string_to_value(Arc::new(|text| parse_ratio(text, true)))
}

fn percent_param(name: &'static str, default: f32) -> FloatParam {
    ratio_param(name, default, 0.0, 1.0)
}

fn ratio_param(name: &'static str, default: f32, min: f32, max: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min, max })
        .with_step_size(RATIO_STEP)
        .with_value_to_string(Arc::new(|value| format!("{:.0}%", value * 100.0)))
        .with_string_to_value(Arc::new(|text| parse_ratio(text, false)))
}

const RATIO_STEP: f32 = 0.01;

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x100000001b3)
}

fn mix_f32(hash: u64, value: f32) -> u64 {
    mix(hash, value.to_bits() as u64)
}

fn mix_float(hash: u64, param: &FloatParam) -> u64 {
    let hash = mix_f32(hash, param.value());
    mix_f32(hash, param.modulated_normalized_value())
}

fn mix_int(hash: u64, param: &IntParam) -> u64 {
    let hash = mix(hash, param.value() as u32 as u64);
    mix_f32(hash, param.modulated_normalized_value())
}

fn parse_ratio(text: &str, signed: bool) -> Option<f32> {
    let trimmed = text.trim().trim_end_matches('%').trim();
    let value = trimmed.parse::<f32>().ok()?;
    let as_percent = if signed {
        value.abs() > 1.5
    } else {
        value > 1.5
    };
    Some(if as_percent { value / 100.0 } else { value })
}

#[cfg(test)]
mod tests {
    use nice_plug::prelude::Params;

    use super::*;

    #[test]
    fn host_defaults_are_the_groove_defaults() {
        assert_eq!(gen_params(&SonomoriParams::default()), GenParams::default());
    }

    #[test]
    fn every_host_param_moves_the_picture_key() {
        let params = SonomoriParams::default();
        let base = params.fingerprint();
        for (id, ptr, _) in params.param_map() {
            // `ptr` points into `params`, which lives for this loop.
            let current = unsafe { ptr.modulated_normalized_value() };
            let mut moved = false;
            for target in [0.0, 1.0] {
                let changed = unsafe { ptr._internal_set_normalized_value(target) };
                if !changed {
                    continue;
                }
                assert_ne!(
                    params.fingerprint(),
                    base,
                    "{id} is missing from the picture key"
                );
                unsafe { ptr._internal_set_normalized_value(current) };
                moved = true;
                break;
            }
            assert!(moved, "{id} could not leave its default");
        }
    }
}
