use std::path::PathBuf;

use crate::{filters::FilterDomain, media::MediaFormat, PreparedTransform, Result};

mod avif;
mod avif_sequence;
pub mod encoded_video;
pub mod image;
pub mod jpeg;
mod jpeg_entropy;
pub(crate) mod matroska_blocks;
pub mod mp3;
pub mod mp3_frames;
pub mod mp4;
pub mod ogg;
pub mod ogg_pages;
pub mod png;
pub mod wav;
mod webp;

pub fn execute(prepared: PreparedTransform) -> Result<PathBuf> {
    match prepared.plan().format {
        MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::WebP | MediaFormat::Avif => {
            image::execute(prepared)
        }
        MediaFormat::Wav => wav::execute(prepared),
        MediaFormat::Mp3 => mp3::execute(prepared),
        MediaFormat::Mp4 | MediaFormat::Matroska => mp4::execute(prepared),
        MediaFormat::Ogg => ogg::execute(prepared),
    }
}

const JPEG_DOMAINS: &[FilterDomain] = &[FilterDomain::JpegHuffmanTables, FilterDomain::ImagePixels];
const PNG_DOMAINS: &[FilterDomain] = &[FilterDomain::EncodedPayload, FilterDomain::ImagePixels];
const WEBP_DOMAINS: &[FilterDomain] = &[FilterDomain::ImagePixels];
const WAV_DOMAINS: &[FilterDomain] = &[FilterDomain::EncodedPayload, FilterDomain::PcmAudio];
const MP3_DOMAINS: &[FilterDomain] = &[FilterDomain::Mp3MainData, FilterDomain::FfmpegAudio];
const OGG_DOMAINS: &[FilterDomain] = &[
    FilterDomain::OggPacket,
    FilterDomain::PcmAudio,
    FilterDomain::FfmpegAudio,
];
const MP4_DOMAINS: &[FilterDomain] = &[
    FilterDomain::EncodedVideoPacket,
    FilterDomain::ImagePixels,
    FilterDomain::PcmAudio,
    FilterDomain::FfmpegAudio,
    FilterDomain::FfmpegVideo,
];
const MATROSKA_DOMAINS: &[FilterDomain] = &[
    FilterDomain::EncodedVideoPacket,
    FilterDomain::ImagePixels,
    FilterDomain::PcmAudio,
    FilterDomain::FfmpegAudio,
    FilterDomain::FfmpegVideo,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecCapabilities {
    pub format: MediaFormat,
    pub domains: &'static [FilterDomain],
    pub requires_ffmpeg: bool,
}

impl CodecCapabilities {
    pub fn supports(self, domain: FilterDomain) -> bool {
        self.domains.contains(&domain)
    }
}

pub const fn capabilities_for(format: MediaFormat) -> CodecCapabilities {
    match format {
        MediaFormat::Jpeg => CodecCapabilities {
            format,
            domains: JPEG_DOMAINS,
            requires_ffmpeg: false,
        },
        MediaFormat::Png => CodecCapabilities {
            format,
            domains: PNG_DOMAINS,
            requires_ffmpeg: false,
        },
        MediaFormat::Wav => CodecCapabilities {
            format,
            domains: WAV_DOMAINS,
            requires_ffmpeg: false,
        },
        MediaFormat::Mp3 => CodecCapabilities {
            format,
            domains: MP3_DOMAINS,
            requires_ffmpeg: true,
        },
        MediaFormat::Mp4 => CodecCapabilities {
            format,
            domains: MP4_DOMAINS,
            requires_ffmpeg: true,
        },
        MediaFormat::WebP => CodecCapabilities {
            format,
            domains: WEBP_DOMAINS,
            requires_ffmpeg: false,
        },
        MediaFormat::Matroska => CodecCapabilities {
            format,
            domains: MATROSKA_DOMAINS,
            requires_ffmpeg: true,
        },
        MediaFormat::Avif => CodecCapabilities {
            format,
            domains: WEBP_DOMAINS,
            requires_ffmpeg: false,
        },
        MediaFormat::Ogg => CodecCapabilities {
            format,
            domains: OGG_DOMAINS,
            requires_ffmpeg: true,
        },
    }
}

pub fn is_available(format: MediaFormat) -> bool {
    if !capabilities_for(format).requires_ffmpeg {
        return true;
    }
    let runner = crate::ffmpeg::ToolRunner::default();
    if !runner.ffmpeg_available() || !runner.ffprobe_available() {
        return false;
    }
    required_encoders(format)
        .iter()
        .all(|encoder| runner.supports_encoder(encoder))
        && required_filters(format)
            .iter()
            .all(|filter| runner.supports_filter(filter))
}

pub(crate) fn runtime_unavailable_reason(
    format: MediaFormat,
    filter: &crate::FilterSpec,
) -> Option<String> {
    runtime_unavailable_reason_with_runner(format, filter, &crate::ffmpeg::ToolRunner::default())
}

fn runtime_unavailable_reason_with_runner(
    format: MediaFormat,
    filter: &crate::FilterSpec,
    runner: &crate::ffmpeg::ToolRunner,
) -> Option<String> {
    if !capabilities_for(format).requires_ffmpeg {
        return None;
    }
    if !runner.ffmpeg_available() {
        return Some("configured FFmpeg executable is unavailable".to_owned());
    }
    if !runner.ffprobe_available() {
        return Some("configured ffprobe executable is unavailable".to_owned());
    }
    if let Some(encoder) = required_encoders(format)
        .iter()
        .find(|encoder| !runner.supports_encoder(encoder))
    {
        return Some(format!("required FFmpeg encoder {encoder} is unavailable"));
    }
    if let Some(graph) = filter.expert_graph() {
        return graph
            .filters()
            .iter()
            .find(|name| !runner.supports_filter(name))
            .map(|name| format!("expert graph requires unavailable FFmpeg filter {name}"));
    }
    let ffmpeg_filter = match filter.name() {
        "high-pass" => Some("highpass"),
        "low-pass" => Some("lowpass"),
        "echo" => Some("aecho"),
        "volume" => Some("volume"),
        "hue" => Some("hue"),
        "equalize" => Some("eq"),
        "lag" => Some("tmix"),
        _ => None,
    };
    ffmpeg_filter
        .filter(|name| !runner.supports_filter(name))
        .map(|name| format!("required FFmpeg filter {name} is unavailable"))
}

fn required_encoders(format: MediaFormat) -> &'static [&'static str] {
    match format {
        MediaFormat::Mp3 => &["libmp3lame"],
        MediaFormat::Ogg => &["libvorbis", "libopus", "pcm_s16le"],
        MediaFormat::Mp4 => &["mpeg4", "aac", "ffv1", "pcm_s16le"],
        MediaFormat::Matroska => &["ffv1", "flac", "pcm_s16le"],
        _ => &[],
    }
}

fn required_filters(format: MediaFormat) -> &'static [&'static str] {
    match format {
        MediaFormat::Mp3 | MediaFormat::Ogg => &["highpass", "lowpass", "aecho", "volume"],
        MediaFormat::Mp4 | MediaFormat::Matroska => &[
            "highpass", "lowpass", "aecho", "volume", "hue", "eq", "tmix",
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_configured_tool_failure_during_filter_preflight() {
        let runner = crate::ffmpeg::ToolRunner::new("missing-ffmpeg", "missing-ffprobe");
        let reason = runtime_unavailable_reason_with_runner(
            MediaFormat::Mp3,
            &crate::FilterSpec::parse("volume").unwrap(),
            &runner,
        )
        .unwrap();

        assert_eq!(reason, "configured FFmpeg executable is unavailable");
    }
}
