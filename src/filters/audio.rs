use crate::{AudioEffect, DatabenderError, FilterSpec, Result};

pub fn compile_audio_graph(filters: &[FilterSpec]) -> Result<String> {
    filters
        .iter()
        .map(|filter| match filter {
            FilterSpec::AudioEffect(AudioEffect::HighPass { frequency }) => {
                Ok(format!("highpass=f={frequency}"))
            }
            FilterSpec::AudioEffect(AudioEffect::LowPass { frequency }) => {
                Ok(format!("lowpass=f={frequency}"))
            }
            FilterSpec::AudioEffect(AudioEffect::Echo { delay_ms, decay }) => {
                Ok(format!("aecho=0.8:0.9:{delay_ms}:{decay}"))
            }
            FilterSpec::AudioEffect(AudioEffect::Volume { gain }) => Ok(format!("volume={gain}")),
            _ => Err(DatabenderError::OutputValidation {
                reason: format!(
                    "filter {} cannot be compiled into an FFmpeg audio graph",
                    filter.name()
                ),
            }),
        })
        .collect::<Result<Vec<_>>>()
        .map(|filters| filters.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_typed_effects_in_order() {
        let filters = [
            FilterSpec::AudioEffect(AudioEffect::HighPass { frequency: 200 }),
            FilterSpec::AudioEffect(AudioEffect::LowPass { frequency: 3_000 }),
            FilterSpec::AudioEffect(AudioEffect::Echo {
                delay_ms: 250,
                decay: 0.4,
            }),
            FilterSpec::AudioEffect(AudioEffect::Volume { gain: 1.5 }),
        ];

        assert_eq!(
            compile_audio_graph(&filters).unwrap(),
            "highpass=f=200,lowpass=f=3000,aecho=0.8:0.9:250:0.4,volume=1.5"
        );
    }

    #[test]
    fn rejects_filters_outside_the_ffmpeg_audio_domain() {
        let error = compile_audio_graph(&[FilterSpec::AudioNoise {
            probability: 0.1,
            amplitude: 0.2,
        }])
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot be compiled into an FFmpeg audio graph"));
    }
}
