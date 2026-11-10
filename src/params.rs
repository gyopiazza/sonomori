//! Host parameters. The pattern generator reads these through [`gen_params`]
//! and does not depend on this module.

use std::sync::Arc;

use nice_plug::prelude::*;
use parking_lot::Mutex;

use crate::pattern::GenParams;
use crate::score::{BARS_MAX, BARS_MIN, SWING_MAX, SWING_MIN};

/// Inclusive seed the host parameter and the Randomize button both use.
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
        Self {
            density: percent_param("Density", 0.62),
            syncopation: bipolar_param("Syncopation", 0.1),
            symmetry: percent_param("Symmetry", 0.55),
            pattern_var: percent_param("Pattern Variation", 0.2),
            sound_var: percent_param("Sound Variation", 0.25),
            swing: ratio_param("Swing", SWING_MIN, SWING_MIN, SWING_MAX),
            decay: percent_param("Decay", 0.55),
            pitch: IntParam::new("Pitch", 0, IntRange::Linear { min: -12, max: 12 })
                .with_unit(" st"),
            tone: percent_param("Tone", 0.78),
            sensitivity: percent_param("Sensitivity", 0.45),
            bars: IntParam::new(
                "Bars",
                BARS_MIN as i32,
                IntRange::Linear {
                    min: BARS_MIN as i32,
                    max: BARS_MAX as i32,
                },
            ),
            tempo: FloatParam::new(
                "Audition BPM",
                120.0,
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
                SEED_MIN,
                IntRange::Linear {
                    min: SEED_MIN,
                    max: SEED_MAX,
                },
            ),
            sample_path: Mutex::new(String::new()),
        }
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

/// `signed` treats a magnitude above 1.5 as a percent, so a negative entry still scales.
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
