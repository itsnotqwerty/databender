use std::{collections::BTreeMap, fmt::Display, str::FromStr};

use crate::media::StreamKind;

use crate::{DatabenderError, Result};

pub mod audio;
pub mod bytes;
pub mod graph;
pub mod image;
pub mod mp3;
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
pub const MP3_ENCODED_FILTER_NAMES: [&str; 1] = ["mp3-main-data-noise"];
pub const OGG_ENCODED_FILTER_NAMES: [&str; 1] = ["ogg-packet-noise"];
pub const VIDEO_ENCODED_FILTER_NAMES: [&str; 1] = ["video-packet-noise"];
pub const FFMPEG_AUDIO_FILTER_NAMES: [&str; 5] = [
    "high-pass",
    "low-pass",
    "echo",
    "volume",
    "expert-audio-graph",
];
pub const AUDIO_FILTER_NAMES: [&str; 6] = [
    "audio-noise",
    "high-pass",
    "low-pass",
    "echo",
    "volume",
    "expert-audio-graph",
];
pub const VIDEO_FILTER_NAMES: [&str; 4] = ["hue", "equalize", "lag", "expert-video-graph"];

pub const FILTER_NAMES: [&str; 28] = [
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
    "mp3-main-data-noise",
    "ogg-packet-noise",
    "video-packet-noise",
    "high-pass",
    "low-pass",
    "echo",
    "volume",
    "hue",
    "equalize",
    "lag",
    "expert-audio-graph",
    "expert-video-graph",
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FilterDomain {
    JpegHuffmanTables,
    EncodedPayload,
    ImagePixels,
    PcmAudio,
    Mp3MainData,
    OggPacket,
    EncodedVideoPacket,
    FfmpegAudio,
    FfmpegVideo,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterSpec {
    HuffmanGlitch {
        swaps: usize,
        intensity: f64,
        target: HuffmanTarget,
        engine: HuffmanGlitchEngine,
        mode: HuffmanGlitchMode,
        preserve_size: bool,
        scan_start: usize,
        scan_count: usize,
        frequency_start: usize,
        frequency_end: usize,
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
    Mp3MainDataNoise {
        byte_budget: usize,
        start_frame: usize,
        frame_count: usize,
        intensity: f64,
    },
    OggPacketNoise {
        byte_budget: usize,
        start_packet: usize,
        packet_count: usize,
        intensity: f64,
        max_decode_errors: usize,
    },
    VideoPacketNoise {
        byte_budget: usize,
        start_packet: usize,
        packet_count: usize,
        frame_type: VideoPacketFrameType,
        intensity: f64,
        max_frame_loss: usize,
    },
    AudioEffect(AudioEffect),
    VideoEffect(VideoEffect),
    ExpertAudioGraph(graph::ExpertGraph),
    ExpertVideoGraph(graph::ExpertGraph),
}

impl FilterSpec {
    pub fn parse(specification: &str) -> Result<Self> {
        if let Some(fragment) = specification.strip_prefix("expert-audio-graph:") {
            return graph::ExpertGraph::parse(fragment).map(Self::ExpertAudioGraph);
        }
        if let Some(fragment) = specification.strip_prefix("expert-video-graph:") {
            return graph::ExpertGraph::parse(fragment).map(Self::ExpertVideoGraph);
        }
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
                engine: parameters.value("engine", HuffmanGlitchEngine::Table)?,
                mode: parameters.value("mode", HuffmanGlitchMode::RunRemap)?,
                preserve_size: parameters.value("preserve_size", true)?,
                scan_start: parameters.bounded("scan_start", 0, 0, 65_535)?,
                scan_count: parameters.bounded("scan_count", 0, 0, 65_535)?,
                frequency_start: parameters.bounded("frequency_start", 1, 1, 63)?,
                frequency_end: parameters.bounded("frequency_end", 63, 1, 63)?,
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
            "mp3-main-data-noise" => Self::Mp3MainDataNoise {
                byte_budget: parameters.bounded("byte_budget", 8, 1, 1_048_576)?,
                start_frame: parameters.bounded("start_frame", 0, 0, 1_048_576)?,
                frame_count: parameters.bounded("frame_count", 0, 0, 1_048_576)?,
                intensity: parameters.bounded("intensity", 0.125, 0.0, 1.0)?,
            },
            "ogg-packet-noise" => Self::OggPacketNoise {
                byte_budget: parameters.bounded("byte_budget", 8, 1, 1_048_576)?,
                start_packet: parameters.bounded("start_packet", 0, 0, 1_048_576)?,
                packet_count: parameters.bounded("packet_count", 0, 0, 1_048_576)?,
                intensity: parameters.bounded("intensity", 0.125, 0.0, 1.0)?,
                max_decode_errors: parameters.bounded("max_decode_errors", 0, 0, 1_048_576)?,
            },
            "video-packet-noise" => Self::VideoPacketNoise {
                byte_budget: parameters.bounded("byte_budget", 8, 1, 1_048_576)?,
                start_packet: parameters.bounded("start_packet", 0, 0, 1_048_576)?,
                packet_count: parameters.bounded("packet_count", 0, 0, 1_048_576)?,
                frame_type: parameters.value("frame_type", VideoPacketFrameType::All)?,
                intensity: parameters.bounded("intensity", 0.125, 0.0, 1.0)?,
                max_frame_loss: parameters.bounded("max_frame_loss", 0, 0, 1_048_576)?,
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

        if let Self::HuffmanGlitch {
            frequency_start,
            frequency_end,
            ..
        } = &filter
        {
            if frequency_start > frequency_end {
                return Err(invalid_parameter(
                    name,
                    "frequency_start",
                    "must not exceed frequency_end",
                ));
            }
        }
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
            Self::Mp3MainDataNoise { .. } => "mp3-main-data-noise",
            Self::OggPacketNoise { .. } => "ogg-packet-noise",
            Self::VideoPacketNoise { .. } => "video-packet-noise",
            Self::AudioEffect(effect) => effect.name(),
            Self::VideoEffect(effect) => effect.name(),
            Self::ExpertAudioGraph(_) => "expert-audio-graph",
            Self::ExpertVideoGraph(_) => "expert-video-graph",
        }
    }

    pub fn specification(&self) -> String {
        match self {
            Self::HuffmanGlitch {
                swaps,
                intensity,
                target,
                engine,
                mode,
                preserve_size,
                scan_start,
                scan_count,
                frequency_start,
                frequency_end,
            } => {
                let target_name = match target {
                    HuffmanTarget::All => "all",
                    HuffmanTarget::LumaAc => "luma-ac",
                    HuffmanTarget::ChromaAc => "chroma-ac",
                };
                let mode_name = match mode {
                    HuffmanGlitchMode::RunRemap => "run-remap",
                    HuffmanGlitchMode::SymbolRemap => "symbol-remap",
                };
                let engine_name = match engine {
                    HuffmanGlitchEngine::Table => "table",
                    HuffmanGlitchEngine::Coefficient => "coefficient",
                };
                filter_specification(
                    "huffman-glitch",
                    [
                        (*swaps != 32).then(|| format!("swaps={swaps}")),
                        (*intensity != 1.0).then(|| format!("intensity={intensity}")),
                        (*target != HuffmanTarget::LumaAc).then(|| format!("target={target_name}")),
                        (*engine != HuffmanGlitchEngine::Table)
                            .then(|| format!("engine={engine_name}")),
                        (*mode != HuffmanGlitchMode::RunRemap).then(|| format!("mode={mode_name}")),
                        (!*preserve_size).then(|| format!("preserve_size={preserve_size}")),
                        (*scan_start != 0).then(|| format!("scan_start={scan_start}")),
                        (*scan_count != 0).then(|| format!("scan_count={scan_count}")),
                        (*frequency_start != 1)
                            .then(|| format!("frequency_start={frequency_start}")),
                        (*frequency_end != 63).then(|| format!("frequency_end={frequency_end}")),
                    ],
                )
            }
            Self::ByteNoise { probability } => filter_specification(
                "byte-noise",
                [(*probability != 0.05).then(|| format!("probability={probability}"))],
            ),
            Self::ByteRepeat { count } => filter_specification(
                "byte-repeat",
                [(*count != 8).then(|| format!("count={count}"))],
            ),
            Self::ByteDrop { count } => filter_specification(
                "byte-drop",
                [(*count != 8).then(|| format!("count={count}"))],
            ),
            Self::ByteSwap { count } => filter_specification(
                "byte-swap",
                [(*count != 8).then(|| format!("count={count}"))],
            ),
            Self::ChannelShift { pixels } => filter_specification(
                "channel-shift",
                [(*pixels != 4).then(|| format!("pixels={pixels}"))],
            ),
            Self::ScanlineDisplacement { max_shift } => filter_specification(
                "scanline-displacement",
                [(*max_shift != 12).then(|| format!("max_shift={max_shift}"))],
            ),
            Self::PixelSort { threshold } => filter_specification(
                "pixel-sort",
                [(*threshold != 128).then(|| format!("threshold={threshold}"))],
            ),
            Self::Brightness { delta } => filter_specification(
                "brightness",
                [(*delta != 24).then(|| format!("delta={delta}"))],
            ),
            Self::Contrast { factor } => filter_specification(
                "contrast",
                [(*factor != 1.25).then(|| format!("factor={factor}"))],
            ),
            Self::Saturation { factor } => filter_specification(
                "saturation",
                [(*factor != 1.5).then(|| format!("factor={factor}"))],
            ),
            Self::HueRotate { degrees } => filter_specification(
                "hue-rotate",
                [(*degrees != 45.0).then(|| format!("degrees={degrees}"))],
            ),
            Self::Posterize { bits } => {
                filter_specification("posterize", [(*bits != 4).then(|| format!("bits={bits}"))])
            }
            Self::Invert => "invert".to_owned(),
            Self::RowDropout { probability } => filter_specification(
                "row-dropout",
                [(*probability != 0.1).then(|| format!("probability={probability}"))],
            ),
            Self::AudioNoise {
                probability,
                amplitude,
            } => filter_specification(
                "audio-noise",
                [
                    (*probability != 0.05).then(|| format!("probability={probability}")),
                    (*amplitude != 0.1).then(|| format!("amplitude={amplitude}")),
                ],
            ),
            Self::Mp3MainDataNoise {
                byte_budget,
                start_frame,
                frame_count,
                intensity,
            } => filter_specification(
                "mp3-main-data-noise",
                [
                    (*byte_budget != 8).then(|| format!("byte_budget={byte_budget}")),
                    (*start_frame != 0).then(|| format!("start_frame={start_frame}")),
                    (*frame_count != 0).then(|| format!("frame_count={frame_count}")),
                    (*intensity != 0.125).then(|| format!("intensity={intensity}")),
                ],
            ),
            Self::OggPacketNoise {
                byte_budget,
                start_packet,
                packet_count,
                intensity,
                max_decode_errors,
            } => filter_specification(
                "ogg-packet-noise",
                [
                    (*byte_budget != 8).then(|| format!("byte_budget={byte_budget}")),
                    (*start_packet != 0).then(|| format!("start_packet={start_packet}")),
                    (*packet_count != 0).then(|| format!("packet_count={packet_count}")),
                    (*intensity != 0.125).then(|| format!("intensity={intensity}")),
                    (*max_decode_errors != 0)
                        .then(|| format!("max_decode_errors={max_decode_errors}")),
                ],
            ),
            Self::VideoPacketNoise {
                byte_budget,
                start_packet,
                packet_count,
                frame_type,
                intensity,
                max_frame_loss,
            } => filter_specification(
                "video-packet-noise",
                [
                    (*byte_budget != 8).then(|| format!("byte_budget={byte_budget}")),
                    (*start_packet != 0).then(|| format!("start_packet={start_packet}")),
                    (*packet_count != 0).then(|| format!("packet_count={packet_count}")),
                    (*frame_type != VideoPacketFrameType::All)
                        .then(|| format!("frame_type={frame_type}")),
                    (*intensity != 0.125).then(|| format!("intensity={intensity}")),
                    (*max_frame_loss != 0).then(|| format!("max_frame_loss={max_frame_loss}")),
                ],
            ),
            Self::AudioEffect(AudioEffect::HighPass { frequency }) => filter_specification(
                "high-pass",
                [(*frequency != 200).then(|| format!("frequency={frequency}"))],
            ),
            Self::AudioEffect(AudioEffect::LowPass { frequency }) => filter_specification(
                "low-pass",
                [(*frequency != 3_000).then(|| format!("frequency={frequency}"))],
            ),
            Self::AudioEffect(AudioEffect::Echo { delay_ms, decay }) => filter_specification(
                "echo",
                [
                    (*delay_ms != 250).then(|| format!("delay_ms={delay_ms}")),
                    (*decay != 0.4).then(|| format!("decay={decay}")),
                ],
            ),
            Self::AudioEffect(AudioEffect::Volume { gain }) => {
                filter_specification("volume", [(*gain != 1.0).then(|| format!("gain={gain}"))])
            }
            Self::VideoEffect(VideoEffect::Hue { degrees }) => filter_specification(
                "hue",
                [(*degrees != 30.0).then(|| format!("degrees={degrees}"))],
            ),
            Self::VideoEffect(VideoEffect::Equalize { contrast }) => filter_specification(
                "equalize",
                [(*contrast != 1.2).then(|| format!("contrast={contrast}"))],
            ),
            Self::VideoEffect(VideoEffect::Lag { frames }) => {
                filter_specification("lag", [(*frames != 2).then(|| format!("frames={frames}"))])
            }
            Self::ExpertAudioGraph(graph) => {
                format!("expert-audio-graph:{}", graph.fragment())
            }
            Self::ExpertVideoGraph(graph) => {
                format!("expert-video-graph:{}", graph.fragment())
            }
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
            Self::Mp3MainDataNoise { .. } => FilterDomain::Mp3MainData,
            Self::OggPacketNoise { .. } => FilterDomain::OggPacket,
            Self::VideoPacketNoise { .. } => FilterDomain::EncodedVideoPacket,
            Self::AudioEffect(_) => FilterDomain::FfmpegAudio,
            Self::VideoEffect(_) => FilterDomain::FfmpegVideo,
            Self::ExpertAudioGraph(_) => FilterDomain::FfmpegAudio,
            Self::ExpertVideoGraph(_) => FilterDomain::FfmpegVideo,
        }
    }

    pub fn expert_graph(&self) -> Option<&graph::ExpertGraph> {
        match self {
            Self::ExpertAudioGraph(graph) | Self::ExpertVideoGraph(graph) => Some(graph),
            _ => None,
        }
    }

    pub const fn environment_dependent(&self) -> bool {
        matches!(self, Self::ExpertAudioGraph(_) | Self::ExpertVideoGraph(_))
    }

    pub fn impact_estimate(&self) -> Option<String> {
        match self {
            Self::Mp3MainDataNoise {
                byte_budget,
                start_frame,
                frame_count,
                intensity,
            } => {
                let frames = if *frame_count == 0 {
                    format!("frame {start_frame} onward")
                } else {
                    format!(
                        "frames {start_frame} through {}",
                        start_frame.saturating_add(*frame_count).saturating_sub(1)
                    )
                };
                let bits = (*intensity * 8.0).ceil() as usize;
                Some(format!(
                    "up to {byte_budget} main-data bytes in {frames}, flipping up to {bits} bits per byte"
                ))
            }
            Self::OggPacketNoise {
                byte_budget,
                start_packet,
                packet_count,
                intensity,
                max_decode_errors,
            } => {
                let packets = if *packet_count == 0 {
                    format!("audio packet {start_packet} onward")
                } else {
                    format!(
                        "audio packets {start_packet} through {}",
                        start_packet.saturating_add(*packet_count).saturating_sub(1)
                    )
                };
                let bits = (*intensity * 8.0).ceil() as usize;
                Some(format!(
                    "up to {byte_budget} payload bytes in {packets}, flipping up to {bits} bits per byte; tolerate {max_decode_errors} decoder error lines"
                ))
            }
            Self::VideoPacketNoise {
                byte_budget,
                start_packet,
                packet_count,
                frame_type,
                intensity,
                max_frame_loss,
            } => {
                let packets = if *packet_count == 0 {
                    format!("video packet {start_packet} onward")
                } else {
                    format!(
                        "video packets {start_packet} through {}",
                        start_packet.saturating_add(*packet_count).saturating_sub(1)
                    )
                };
                let bits = (*intensity * 8.0).ceil() as usize;
                Some(format!(
                    "up to {byte_budget} protected-payload bytes per {frame_type} {packets}, flipping up to {bits} bits per byte; tolerate {max_frame_loss} lost frames"
                ))
            }
            _ => None,
        }
    }

    pub const fn target(&self) -> StreamKind {
        match self.domain() {
            FilterDomain::JpegHuffmanTables
            | FilterDomain::EncodedPayload
            | FilterDomain::ImagePixels => StreamKind::Image,
            FilterDomain::PcmAudio
            | FilterDomain::Mp3MainData
            | FilterDomain::OggPacket
            | FilterDomain::FfmpegAudio => StreamKind::Audio,
            FilterDomain::EncodedVideoPacket | FilterDomain::FfmpegVideo => StreamKind::Video,
        }
    }
}

fn filter_specification<const N: usize>(name: &str, parameters: [Option<String>; N]) -> String {
    let parameters = parameters.into_iter().flatten().collect::<Vec<_>>();
    if parameters.is_empty() {
        name.to_owned()
    } else {
        format!("{name}:{}", parameters.join(","))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HuffmanTarget {
    All,
    LumaAc,
    ChromaAc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoPacketFrameType {
    All,
    Key,
    Delta,
}

impl Display for VideoPacketFrameType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::All => "all",
            Self::Key => "key",
            Self::Delta => "delta",
        })
    }
}

impl FromStr for VideoPacketFrameType {
    type Err = &'static str;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "key" => Ok(Self::Key),
            "delta" => Ok(Self::Delta),
            _ => Err("expected all, key, or delta"),
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HuffmanGlitchEngine {
    Table,
    Coefficient,
}

impl FromStr for HuffmanGlitchEngine {
    type Err = &'static str;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "table" => Ok(Self::Table),
            "coefficient" => Ok(Self::Coefficient),
            _ => Err("expected table or coefficient"),
        }
    }
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
                "huffman-glitch:swaps=128,intensity=0.5,target=luma-ac,engine=coefficient,mode=symbol-remap,preserve_size=false,scan_start=2,scan_count=1,frequency_start=4,frequency_end=20"
            )
            .unwrap(),
            FilterSpec::HuffmanGlitch {
                swaps: 128,
                intensity: 0.5,
                target: HuffmanTarget::LumaAc,
                engine: HuffmanGlitchEngine::Coefficient,
                mode: HuffmanGlitchMode::SymbolRemap,
                preserve_size: false,
                scan_start: 2,
                scan_count: 1,
                frequency_start: 4,
                frequency_end: 20,
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
        assert_eq!(
            FilterSpec::parse(
                "video-packet-noise:byte_budget=12,start_packet=3,packet_count=4,frame_type=delta,intensity=0.25,max_frame_loss=2"
            )
            .unwrap(),
            FilterSpec::VideoPacketNoise {
                byte_budget: 12,
                start_packet: 3,
                packet_count: 4,
                frame_type: VideoPacketFrameType::Delta,
                intensity: 0.25,
                max_frame_loss: 2,
            }
        );
    }

    #[test]
    fn retains_defaults_for_bare_filter_names() {
        assert_eq!(
            FilterSpec::parse("huffman-glitch").unwrap(),
            FilterSpec::HuffmanGlitch {
                swaps: 32,
                intensity: 1.0,
                target: HuffmanTarget::LumaAc,
                engine: HuffmanGlitchEngine::Table,
                mode: HuffmanGlitchMode::RunRemap,
                preserve_size: true,
                scan_start: 0,
                scan_count: 0,
                frequency_start: 1,
                frequency_end: 63,
            }
        );
        assert_eq!(
            FilterSpec::parse("channel-shift").unwrap(),
            FilterSpec::ChannelShift { pixels: 4 }
        );
        assert_eq!(
            FilterSpec::parse("video-packet-noise").unwrap(),
            FilterSpec::VideoPacketNoise {
                byte_budget: 8,
                start_packet: 0,
                packet_count: 0,
                frame_type: VideoPacketFrameType::All,
                intensity: 0.125,
                max_frame_loss: 0,
            }
        );
    }

    #[test]
    fn canonical_specifications_round_trip_typed_values() {
        for specification in [
            "huffman-glitch:swaps=128,intensity=0.5,target=chroma-ac,engine=table,mode=symbol-remap,preserve_size=false",
            "channel-shift:pixels=-12",
            "audio-noise:probability=0.25,amplitude=0.8",
            "ogg-packet-noise:byte_budget=12,start_packet=3,packet_count=4,intensity=0.25,max_decode_errors=2",
            "echo:delay_ms=400,decay=0.6",
            "expert-video-graph:hue=h=30,eq=contrast=1.2",
        ] {
            let filter = FilterSpec::parse(specification).unwrap();
            assert_eq!(FilterSpec::parse(&filter.specification()).unwrap(), filter);
        }
    }

    #[test]
    fn canonical_specifications_hide_default_options() {
        for name in [
            "huffman-glitch",
            "channel-shift",
            "audio-noise",
            "ogg-packet-noise",
            "echo",
            "hue",
        ] {
            assert_eq!(FilterSpec::parse(name).unwrap().specification(), name);
        }
        assert_eq!(
            FilterSpec::parse("audio-noise:amplitude=0.8")
                .unwrap()
                .specification(),
            "audio-noise:amplitude=0.8"
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        let error = FilterSpec::parse("byte-noise:probability=1.1").unwrap_err();
        assert!(matches!(error, DatabenderError::InvalidParameter { .. }));
        assert!(FilterSpec::parse("byte-noise:probability=NaN").is_err());
        assert!(FilterSpec::parse("posterize:bits=9").is_err());
        assert!(FilterSpec::parse("row-dropout:probability=-0.1").is_err());
        assert!(FilterSpec::parse("mp3-main-data-noise:byte_budget=0").is_err());
        assert!(FilterSpec::parse("mp3-main-data-noise:intensity=1.1").is_err());
        assert!(FilterSpec::parse("ogg-packet-noise:byte_budget=0").is_err());
        assert!(FilterSpec::parse("ogg-packet-noise:intensity=1.1").is_err());
        assert!(FilterSpec::parse("huffman-glitch:engine=spatial").is_err());
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
