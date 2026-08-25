use std::path::PathBuf;

use crate::{filters::FilterDomain, media::MediaFormat, PreparedTransform, Result};

pub mod image;
pub mod jpeg;
mod jpeg_entropy;
pub mod mp3;
pub mod mp4;
pub mod ogg;
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
const MP3_DOMAINS: &[FilterDomain] = &[FilterDomain::FfmpegAudio];
const OGG_DOMAINS: &[FilterDomain] = &[FilterDomain::PcmAudio, FilterDomain::FfmpegAudio];
const MP4_DOMAINS: &[FilterDomain] = &[
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
            domains: MP4_DOMAINS,
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
