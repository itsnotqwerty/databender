use std::{collections::BTreeMap, fmt::Display, str::FromStr};

use crate::media::StreamKind;

use crate::{DatabenderError, Result};

pub mod audio;
pub mod bytes;
pub mod image;
pub mod video;

pub const IMAGE_FILTER_NAMES: [&str; 10] = [
    "channel-shift",
    "scanline-displacement",
    "pixel-sort",
    "brightness",
    "contrast",
    "saturation",
    "hue-rotate",
    "posterize",
    "invert",
    "row-dropout",
];
pub const PAYLOAD_FILTER_NAMES: [&str; 4] = ["byte-noise", "byte-repeat", "byte-drop", "byte-swap"];
pub const JPEG_HUFFMAN_FILTER_NAMES: [&str; 1] = ["huffman-glitch"];
pub const PCM_AUDIO_FILTER_NAMES: [&str; 1] = ["audio-noise"];
pub const FFMPEG_AUDIO_FILTER_NAMES: [&str; 4] = ["high-pass", "low-pass", "echo", "volume"];
pub const AUDIO_FILTER_NAMES: [&str; 5] =
    ["audio-noise", "high-pass", "low-pass", "echo", "volume"];
pub const VIDEO_FILTER_NAMES: [&str; 3] = ["hue", "equalize", "lag"];

pub const FILTER_NAMES: [&str; 23] = [
    "huffman-glitch",
    "byte-noise",
    "byte-repeat",
    "byte-drop",
    "byte-swap",
    "channel-shift",
    "scanline-displacement",
    "pixel-sort",
    "brightness",
    "contrast",
    "saturation",
    "hue-rotate",
    "posterize",
    "invert",
    "row-dropout",
    "audio-noise",
    "high-pass",
    "low-pass",
    "echo",
    "volume",
    "hue",
    "equalize",
    "lag",
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FilterDomain {
    JpegHuffmanTables,
    EncodedPayload,
    ImagePixels,
    PcmAudio,
    FfmpegAudio,
    FfmpegVideo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterSpec {
    HuffmanGlitch {
        swaps: usize,
        intensity: f64,
        target: HuffmanTarget,
        mode: HuffmanGlitchMode,
        preserve_size: bool,
    },
    ByteNoise {
        probability: f64,
    },
    ByteRepeat {
        count: usize,
    },
    ByteDrop {
        count: usize,
    },
    ByteSwap {
        count: usize,
    },
    ChannelShift {
        pixels: i32,
    },
    ScanlineDisplacement {
        max_shift: u32,
    },
    PixelSort {
        threshold: u8,
    },
    Brightness {
        delta: i16,
    },
    Contrast {
        factor: f64,
    },
    Saturation {
        factor: f64,
    },
    HueRotate {
        degrees: f64,
    },
    Posterize {
        bits: u8,
    },
    Invert,
    RowDropout {
        probability: f64,
    },
    AudioNoise {
        probability: f64,
        amplitude: f64,
    },
    AudioEffect(AudioEffect),
    VideoEffect(VideoEffect),
}

impl FilterSpec {
    pub fn parse(specification: &str) -> Result<Self> {
        let (name, encoded_parameters) = specification
            .split_once(':')
            .map_or((specification, None), |(name, parameters)| {
                (name, Some(parameters))
            });
        let mut parameters = Parameters::parse(name, encoded_parameters)?;

        let filter = match name {
            "huffman-glitch" => Self::HuffmanGlitch {
                swaps: parameters.bounded("swaps", 32, 1, 1_048_576)?,
                intensity: parameters.bounded("intensity", 1.0, 0.0, 1.0)?,
                target: parameters.value("target", HuffmanTarget::LumaAc)?,
                mode: parameters.value("mode", HuffmanGlitchMode::RunRemap)?,
                preserve_size: parameters.value("preserve_size", true)?,
            },
            "byte-noise" => Self::ByteNoise {
                probability: parameters.bounded("probability", 0.05, 0.0, 1.0)?,
            },
            "byte-repeat" => Self::ByteRepeat {
                count: parameters.bounded("count", 8, 1, 1_048_576)?,
            },
            "byte-drop" => Self::ByteDrop {
                count: parameters.bounded("count", 8, 1, 1_048_576)?,
            },
            "byte-swap" => Self::ByteSwap {
                count: parameters.bounded("count", 8, 1, 1_048_576)?,
            },
            "channel-shift" => Self::ChannelShift {
                pixels: parameters.bounded("pixels", 4, -32_768, 32_768)?,
            },
            "scanline-displacement" => Self::ScanlineDisplacement {
                max_shift: parameters.bounded("max_shift", 12, 1, 32_768)?,
            },
            "pixel-sort" => Self::PixelSort {
                threshold: parameters.bounded("threshold", 128, 0, 255)?,
            },
            "brightness" => Self::Brightness {
                delta: parameters.bounded("delta", 24_i16, -255, 255)?,
            },
            "contrast" => Self::Contrast {
                factor: parameters.bounded("factor", 1.25, 0.0, 4.0)?,
            },
            "saturation" => Self::Saturation {
                factor: parameters.bounded("factor", 1.5, 0.0, 4.0)?,
            },
            "hue-rotate" => Self::HueRotate {
                degrees: parameters.bounded("degrees", 45.0, -180.0, 180.0)?,
            },
            "posterize" => Self::Posterize {
                bits: parameters.bounded("bits", 4_u8, 1, 8)?,
            },
            "invert" => Self::Invert,
            "row-dropout" => Self::RowDropout {
                probability: parameters.bounded("probability", 0.1, 0.0, 1.0)?,
            },
            "audio-noise" => Self::AudioNoise {
                probability: parameters.bounded("probability", 0.05, 0.0, 1.0)?,
                amplitude: parameters.bounded("amplitude", 0.1, 0.0, 1.0)?,
            },
            "high-pass" => Self::AudioEffect(AudioEffect::HighPass {
                frequency: parameters.bounded("frequency", 200_u32, 20, 20_000)?,
            }),
            "low-pass" => Self::AudioEffect(AudioEffect::LowPass {
                frequency: parameters.bounded("frequency", 3_000_u32, 20, 20_000)?,
            }),
            "echo" => Self::AudioEffect(AudioEffect::Echo {
                delay_ms: parameters.bounded("delay_ms", 250_u32, 1, 5_000)?,
                decay: parameters.bounded("decay", 0.4, 0.0, 1.0)?,
            }),
            "volume" => Self::AudioEffect(AudioEffect::Volume {
                gain: parameters.bounded("gain", 1.0, 0.0, 10.0)?,
            }),
            "hue" => Self::VideoEffect(VideoEffect::Hue {
                degrees: parameters.bounded("degrees", 30.0, -180.0, 180.0)?,
            }),
            "equalize" => Self::VideoEffect(VideoEffect::Equalize {
                contrast: parameters.bounded("contrast", 1.2, 0.0, 4.0)?,
            }),
            "lag" => Self::VideoEffect(VideoEffect::Lag {
                frames: parameters.bounded("frames", 2_u32, 1, 300)?,
            }),
            _ => {
                return Err(DatabenderError::UnknownFilter {
                    filter: name.to_owned(),
                })
            }
        };

        parameters.finish()?;
        Ok(filter)
    }

    pub fn named(name: &str) -> Result<Self> {
        Self::parse(name)
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::HuffmanGlitch { .. } => "huffman-glitch",
            Self::ByteNoise { .. } => "byte-noise",
            Self::ByteRepeat { .. } => "byte-repeat",
            Self::ByteDrop { .. } => "byte-drop",
            Self::ByteSwap { .. } => "byte-swap",
            Self::ChannelShift { .. } => "channel-shift",
            Self::ScanlineDisplacement { .. } => "scanline-displacement",
            Self::PixelSort { .. } => "pixel-sort",
            Self::Brightness { .. } => "brightness",
            Self::Contrast { .. } => "contrast",
            Self::Saturation { .. } => "saturation",
            Self::HueRotate { .. } => "hue-rotate",
            Self::Posterize { .. } => "posterize",
            Self::Invert => "invert",
            Self::RowDropout { .. } => "row-dropout",
            Self::AudioNoise { .. } => "audio-noise",
            Self::AudioEffect(effect) => effect.name(),
            Self::VideoEffect(effect) => effect.name(),
        }
    }

    pub const fn domain(&self) -> FilterDomain {
        match self {
            Self::HuffmanGlitch { .. } => FilterDomain::JpegHuffmanTables,
            Self::ByteNoise { .. }
            | Self::ByteRepeat { .. }
            | Self::ByteDrop { .. }
            | Self::ByteSwap { .. } => FilterDomain::EncodedPayload,
            Self::ChannelShift { .. }
            | Self::ScanlineDisplacement { .. }
            | Self::PixelSort { .. }
            | Self::Brightness { .. }
            | Self::Contrast { .. }
            | Self::Saturation { .. }
            | Self::HueRotate { .. }
            | Self::Posterize { .. }
            | Self::Invert
            | Self::RowDropout { .. } => FilterDomain::ImagePixels,
            Self::AudioNoise { .. } => FilterDomain::PcmAudio,
            Self::AudioEffect(_) => FilterDomain::FfmpegAudio,
            Self::VideoEffect(_) => FilterDomain::FfmpegVideo,
        }
    }

    pub const fn target(&self) -> StreamKind {
        match self.domain() {
            FilterDomain::JpegHuffmanTables
            | FilterDomain::EncodedPayload
            | FilterDomain::ImagePixels => StreamKind::Image,
            FilterDomain::PcmAudio | FilterDomain::FfmpegAudio => StreamKind::Audio,
            FilterDomain::FfmpegVideo => StreamKind::Video,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HuffmanTarget {
    All,
    LumaAc,
    ChromaAc,
}

impl FromStr for HuffmanTarget {
    type Err = &'static str;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "luma-ac" => Ok(Self::LumaAc),
            "chroma-ac" => Ok(Self::ChromaAc),
            _ => Err("expected all, luma-ac, or chroma-ac"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HuffmanGlitchMode {
    RunRemap,
    SymbolRemap,
}

impl FromStr for HuffmanGlitchMode {
    type Err = &'static str;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "run-remap" => Ok(Self::RunRemap),
            "symbol-remap" => Ok(Self::SymbolRemap),
            _ => Err("expected run-remap or symbol-remap"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioEffect {
    HighPass { frequency: u32 },
    LowPass { frequency: u32 },
    Echo { delay_ms: u32, decay: f64 },
    Volume { gain: f64 },
}

impl AudioEffect {
    pub const fn name(self) -> &'static str {
        match self {
            Self::HighPass { .. } => "high-pass",
            Self::LowPass { .. } => "low-pass",
            Self::Echo { .. } => "echo",
            Self::Volume { .. } => "volume",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VideoEffect {
    Hue { degrees: f64 },
    Equalize { contrast: f64 },
    Lag { frames: u32 },
}

impl VideoEffect {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hue { .. } => "hue",
            Self::Equalize { .. } => "equalize",
            Self::Lag { .. } => "lag",
        }
    }
}

struct Parameters {
    filter: String,
    values: BTreeMap<String, String>,
}

impl Parameters {
    fn parse(filter: &str, encoded: Option<&str>) -> Result<Self> {
        let mut values = BTreeMap::new();

        if let Some(encoded) = encoded {
            if encoded.is_empty() {
                return Err(invalid_parameter(
                    filter,
                    "parameters",
                    "expected key=value",
                ));
            }

            for assignment in encoded.split(',') {
                let (key, value) = assignment.split_once('=').ok_or_else(|| {
                    invalid_parameter(filter, "parameters", "expected comma-separated key=value")
                })?;
                if key.is_empty() || value.is_empty() {
                    return Err(invalid_parameter(
                        filter,
                        "parameters",
                        "parameter names and values cannot be empty",
                    ));
                }
                if values.insert(key.to_owned(), value.to_owned()).is_some() {
                    return Err(invalid_parameter(
                        filter,
                        key,
                        "parameter was provided twice",
                    ));
                }
            }
        }

        Ok(Self {
            filter: filter.to_owned(),
            values,
        })
    }

    fn bounded<T>(&mut self, key: &str, default: T, minimum: T, maximum: T) -> Result<T>
    where
        T: Copy + Display + FromStr + PartialOrd,
        T::Err: Display,
    {
        let value = match self.values.remove(key) {
            Some(encoded) => encoded.parse::<T>().map_err(|error| {
                invalid_parameter(&self.filter, key, format!("could not parse value: {error}"))
            })?,
            None => default,
        };

        if !(value >= minimum && value <= maximum) {
            return Err(invalid_parameter(
                &self.filter,
                key,
                format!("expected a value from {minimum} through {maximum}"),
            ));
        }

        Ok(value)
    }

    fn value<T>(&mut self, key: &str, default: T) -> Result<T>
    where
        T: FromStr,
        T::Err: Display,
    {
        match self.values.remove(key) {
            Some(encoded) => encoded.parse::<T>().map_err(|error| {
                invalid_parameter(&self.filter, key, format!("could not parse value: {error}"))
            }),
            None => Ok(default),
        }
    }

    fn finish(self) -> Result<()> {
        if let Some(key) = self.values.keys().next() {
            return Err(invalid_parameter(
                &self.filter,
                key,
                "parameter is not supported by this filter",
            ));
        }
        Ok(())
    }
}

fn invalid_parameter(filter: &str, parameter: &str, reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidParameter {
        parameter: format!("{filter}.{parameter}"),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_parameters() {
        assert_eq!(
            FilterSpec::parse(
                "huffman-glitch:swaps=128,intensity=0.5,target=luma-ac,mode=symbol-remap,preserve_size=false"
            )
            .unwrap(),
            FilterSpec::HuffmanGlitch {
                swaps: 128,
                intensity: 0.5,
                target: HuffmanTarget::LumaAc,
                mode: HuffmanGlitchMode::SymbolRemap,
                preserve_size: false,
            }
        );
        assert_eq!(
            FilterSpec::parse("audio-noise:probability=0.25,amplitude=0.8").unwrap(),
            FilterSpec::AudioNoise {
                probability: 0.25,
                amplitude: 0.8,
            }
        );
        assert_eq!(
            FilterSpec::parse("echo:delay_ms=400,decay=0.6").unwrap(),
            FilterSpec::AudioEffect(AudioEffect::Echo {
                delay_ms: 400,
                decay: 0.6,
            })
        );
        assert_eq!(
            FilterSpec::parse("hue-rotate:degrees=-90").unwrap(),
            FilterSpec::HueRotate { degrees: -90.0 }
        );
        assert_eq!(FilterSpec::parse("invert").unwrap(), FilterSpec::Invert);
    }

    #[test]
    fn retains_defaults_for_bare_filter_names() {
        assert_eq!(
            FilterSpec::parse("huffman-glitch").unwrap(),
            FilterSpec::HuffmanGlitch {
                swaps: 32,
                intensity: 1.0,
                target: HuffmanTarget::LumaAc,
                mode: HuffmanGlitchMode::RunRemap,
                preserve_size: true,
            }
        );
        assert_eq!(
            FilterSpec::parse("channel-shift").unwrap(),
            FilterSpec::ChannelShift { pixels: 4 }
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        let error = FilterSpec::parse("byte-noise:probability=1.1").unwrap_err();
        assert!(matches!(error, DatabenderError::InvalidParameter { .. }));
        assert!(FilterSpec::parse("byte-noise:probability=NaN").is_err());
        assert!(FilterSpec::parse("posterize:bits=9").is_err());
        assert!(FilterSpec::parse("row-dropout:probability=-0.1").is_err());
    }

    #[test]
    fn rejects_unknown_and_duplicate_parameters() {
        assert!(FilterSpec::parse("pixel-sort:limit=20").is_err());
        assert!(FilterSpec::parse("pixel-sort:threshold=20,threshold=30").is_err());
    }

    #[test]
    fn rejects_malformed_parameter_lists() {
        assert!(FilterSpec::parse("pixel-sort:").is_err());
        assert!(FilterSpec::parse("pixel-sort:threshold").is_err());
        assert!(FilterSpec::parse("pixel-sort:threshold=").is_err());
    }
}
