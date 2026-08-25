use std::{fmt, fs::File, io::Read, path::Path, str::FromStr};

use crate::{DatabenderError, Result};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaFormat {
    Jpeg,
    Png,
    Wav,
    Mp3,
    Mp4,
    WebP,
    Avif,
    Ogg,
    Matroska,
}

impl MediaFormat {
    pub const ALL: [Self; 9] = [
        Self::Jpeg,
        Self::Png,
        Self::Wav,
        Self::Mp3,
        Self::Mp4,
        Self::WebP,
        Self::Avif,
        Self::Ogg,
        Self::Matroska,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Mp4 => "mp4",
            Self::WebP => "webp",
            Self::Avif => "avif",
            Self::Ogg => "ogg",
            Self::Matroska => "mkv",
        }
    }

    pub fn from_path_extension(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| extension.parse().ok())
    }

    pub fn detect(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|source| DatabenderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut header = [0_u8; 64];
        let bytes_read = file
            .read(&mut header)
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;

        Self::detect_bytes(&header[..bytes_read]).ok_or_else(|| DatabenderError::FormatDetection {
            path: path.to_path_buf(),
        })
    }

    pub fn detect_bytes(header: &[u8]) -> Option<Self> {
        if header.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if header.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WAVE" {
            Some(Self::Wav)
        } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
            Some(Self::WebP)
        } else if header.starts_with(b"ID3")
            || (header.len() >= 2 && header[0] == 0xff && header[1] & 0xe0 == 0xe0)
        {
            Some(Self::Mp3)
        } else if header.starts_with(b"OggS") {
            Some(Self::Ogg)
        } else if header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
            Some(Self::Matroska)
        } else if header.len() >= 12 && &header[4..8] == b"ftyp" {
            if matches!(&header[8..12], b"avif" | b"avis") {
                Some(Self::Avif)
            } else {
                Some(Self::Mp4)
            }
        } else {
            None
        }
    }
}

impl fmt::Display for MediaFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MediaFormat {
    type Err = DatabenderError;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            "wav" | "wave" => Ok(Self::Wav),
            "mp3" => Ok(Self::Mp3),
            "mp4" => Ok(Self::Mp4),
            "webp" => Ok(Self::WebP),
            "avif" => Ok(Self::Avif),
            "ogg" | "oga" | "opus" => Ok(Self::Ogg),
            "mkv" | "matroska" => Ok(Self::Matroska),
            _ => Err(DatabenderError::UnsupportedFormat {
                format: value.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StreamKind {
    Image,
    Audio,
    Video,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_content_signatures() {
        let cases: &[(&[u8], MediaFormat)] = &[
            (&[0xff, 0xd8, 0xff, 0xe0], MediaFormat::Jpeg),
            (b"\x89PNG\r\n\x1a\n", MediaFormat::Png),
            (b"RIFF\x10\x00\x00\x00WAVE", MediaFormat::Wav),
            (b"ID3\x04\x00\x00", MediaFormat::Mp3),
            (&[0xff, 0xfb, 0x90, 0x64], MediaFormat::Mp3),
            (b"\x00\x00\x00\x18ftypisom", MediaFormat::Mp4),
            (b"RIFF\x10\x00\x00\x00WEBP", MediaFormat::WebP),
            (b"\x00\x00\x00\x1cftypavif", MediaFormat::Avif),
            (b"OggS\x00\x02", MediaFormat::Ogg),
            (&[0x1a, 0x45, 0xdf, 0xa3, 0x9f], MediaFormat::Matroska),
        ];

        for (header, expected) in cases {
            assert_eq!(MediaFormat::detect_bytes(header), Some(*expected));
        }
    }

    #[test]
    fn rejects_unknown_or_truncated_content() {
        assert_eq!(MediaFormat::detect_bytes(b"not media"), None);
        assert_eq!(MediaFormat::detect_bytes(b"RIFF"), None);
        assert_eq!(MediaFormat::detect_bytes(b"\x00\x00\x00\x18fty"), None);
    }

    #[test]
    fn recognizes_supported_path_extensions_and_aliases() {
        for (path, expected) in [
            ("image.JPG", MediaFormat::Jpeg),
            ("image.png", MediaFormat::Png),
            ("audio.wave", MediaFormat::Wav),
            ("audio.opus", MediaFormat::Ogg),
            ("video.MATROSKA", MediaFormat::Matroska),
        ] {
            assert_eq!(MediaFormat::from_path_extension(path), Some(expected));
        }
        assert_eq!(MediaFormat::from_path_extension("notes.txt"), None);
        assert_eq!(MediaFormat::from_path_extension("extensionless"), None);
    }
}
