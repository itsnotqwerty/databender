use std::{ffi::OsString, fs, path::Path};

use image::GenericImageView;

use crate::{
    codecs::wav,
    ffmpeg::{ToolRunner, VideoProperties},
    DatabenderError, MediaFormat, Result,
};

const IMAGE_COLUMNS: usize = 48;
const IMAGE_ROWS: usize = 24;
const WAVEFORM_COLUMNS: usize = 48;
const HEX_RAMP: &[u8] = b"0123456789ABCDEF";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewPixel {
    pub character: char,
    pub rgb: [u8; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaPreview {
    Pixels {
        source_width: u32,
        source_height: u32,
        rows: Vec<Vec<PreviewPixel>>,
    },
    Waveform {
        rows: Vec<String>,
    },
    Unavailable,
}

impl MediaPreview {
    pub fn waveform_lines(&self) -> &[String] {
        match self {
            Self::Waveform { rows } => rows,
            Self::Pixels { .. } | Self::Unavailable => &[],
        }
    }
}

pub fn generate(path: impl AsRef<Path>, format: MediaFormat) -> Result<MediaPreview> {
    let path = path.as_ref();
    match format {
        MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::WebP | MediaFormat::Avif => {
            still_image(path)
        }
        MediaFormat::Wav => wav_waveform(path),
        MediaFormat::Mp3 | MediaFormat::Ogg => ffmpeg_waveform(path),
        MediaFormat::Mp4 | MediaFormat::Matroska => ffmpeg_video_frame(path),
    }
}

fn still_image(path: &Path) -> Result<MediaPreview> {
    let image = image::open(path).map_err(|error| DatabenderError::ImageDecode {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let (source_width, source_height) = image.dimensions();
    Ok(pixel_preview(image, source_width, source_height))
}

fn pixel_preview(
    image: image::DynamicImage,
    source_width: u32,
    source_height: u32,
) -> MediaPreview {
    let thumbnail = image
        .thumbnail(IMAGE_COLUMNS as u32, IMAGE_ROWS as u32)
        .into_rgb8();
    let rows = thumbnail
        .rows()
        .map(|row| {
            row.map(|pixel| {
                let luminance = (u16::from(pixel[0]) * 54
                    + u16::from(pixel[1]) * 183
                    + u16::from(pixel[2]) * 19)
                    / 256;
                let index = usize::from(luminance) * (HEX_RAMP.len() - 1) / 255;
                PreviewPixel {
                    character: char::from(HEX_RAMP[index]),
                    rgb: [pixel[0], pixel[1], pixel[2]],
                }
            })
            .collect()
        })
        .collect();
    MediaPreview::Pixels {
        source_width,
        source_height,
        rows,
    }
}

fn wav_waveform(path: &Path) -> Result<MediaPreview> {
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let layout = wav::parse(&encoded)?;
    let bytes_per_sample = layout.format.bytes_per_sample();
    let amplitudes = encoded[layout.data.clone()]
        .chunks_exact(bytes_per_sample)
        .map(|sample| normalized_pcm_amplitude(sample, layout.format.bits_per_sample))
        .collect::<Vec<_>>();
    Ok(MediaPreview::Waveform {
        rows: render_waveform(&amplitudes, WAVEFORM_COLUMNS, 9),
    })
}

fn ffmpeg_waveform(path: &Path) -> Result<MediaPreview> {
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let decoded = workspace.path().join("preview.s16le");
    ToolRunner::default().ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        path.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:a:0"),
        OsString::from("-vn"),
        OsString::from("-ac"),
        OsString::from("1"),
        OsString::from("-ar"),
        OsString::from("8000"),
        OsString::from("-t"),
        OsString::from("10"),
        OsString::from("-f"),
        OsString::from("s16le"),
        decoded.as_os_str().to_owned(),
    ])?;
    let encoded = fs::read(&decoded).map_err(|source| DatabenderError::Io {
        path: decoded,
        source,
    })?;
    let amplitudes = encoded
        .chunks_exact(2)
        .map(|sample| normalized_pcm_amplitude(sample, 16))
        .collect::<Vec<_>>();
    Ok(MediaPreview::Waveform {
        rows: render_waveform(&amplitudes, WAVEFORM_COLUMNS, 9),
    })
}

fn ffmpeg_video_frame(path: &Path) -> Result<MediaPreview> {
    let runner = ToolRunner::default();
    let VideoProperties { width, height } = runner.probe_video(path)?.properties;
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let decoded = workspace.path().join("preview.rgba");
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        path.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-frames:v"),
        OsString::from("1"),
        OsString::from("-pix_fmt"),
        OsString::from("rgba"),
        OsString::from("-f"),
        OsString::from("rawvideo"),
        decoded.as_os_str().to_owned(),
    ])?;
    let pixels = fs::read(&decoded).map_err(|source| DatabenderError::Io {
        path: decoded,
        source,
    })?;
    let image = image::RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
        DatabenderError::OutputValidation {
            reason: "decoded preview frame size does not match probed dimensions".to_owned(),
        }
    })?;
    Ok(pixel_preview(
        image::DynamicImage::ImageRgba8(image),
        width,
        height,
    ))
}

fn normalized_pcm_amplitude(sample: &[u8], bits: u16) -> u32 {
    match bits {
        8 => u32::from(sample[0].abs_diff(128)) << 24,
        16 => u32::from(i16::from_le_bytes(sample.try_into().unwrap()).unsigned_abs()) << 16,
        24 => {
            let value = i32::from_le_bytes([
                sample[0],
                sample[1],
                sample[2],
                if sample[2] & 0x80 == 0 { 0 } else { 0xff },
            ]);
            value.unsigned_abs() << 8
        }
        32 => i32::from_le_bytes(sample.try_into().unwrap()).unsigned_abs(),
        _ => 0,
    }
}

fn render_waveform(amplitudes: &[u32], columns: usize, rows: usize) -> Vec<String> {
    if amplitudes.is_empty() || columns == 0 || rows == 0 {
        return Vec::new();
    }
    let peaks = (0..columns)
        .map(|column| {
            let start = column * amplitudes.len() / columns;
            let end = ((column + 1) * amplitudes.len() / columns).max(start + 1);
            amplitudes[start.min(amplitudes.len() - 1)..end.min(amplitudes.len())]
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    (0..rows)
        .rev()
        .map(|row| {
            peaks
                .iter()
                .map(|peak| {
                    if u64::from(*peak) * rows as u64 > u64::from(u32::MAX) * row as u64 {
                        '#'
                    } else {
                        ' '
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use image::{ImageBuffer, Rgb};

    use super::*;

    #[test]
    fn generates_bounded_still_image_luminance_preview() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        ImageBuffer::from_fn(80, 32, |x, _| {
            let value = (x * 255 / 79) as u8;
            Rgb([value, value, value])
        })
        .save(&path)
        .unwrap();

        let preview = generate(path, MediaFormat::Png).unwrap();

        let MediaPreview::Pixels {
            source_width,
            source_height,
            rows,
        } = preview
        else {
            panic!("expected pixel preview");
        };
        assert_eq!((source_width, source_height), (80, 32));
        assert!(rows.len() <= IMAGE_ROWS);
        assert!(rows.iter().all(|row| row.len() <= IMAGE_COLUMNS));
        assert_ne!(
            rows[0].first().unwrap().character,
            rows[0].last().unwrap().character
        );
        assert!(rows[0].first().unwrap().rgb[0] < 10);
        assert!(rows[0].last().unwrap().rgb[0] > 245);
        assert!(rows
            .iter()
            .flatten()
            .all(|pixel| pixel.character.is_ascii_hexdigit()));
    }

    #[test]
    fn generates_waveform_from_native_pcm_samples() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.wav");
        let samples = [0_i16, i16::MAX, 0, i16::MIN];
        let data_size = (samples.len() * 2) as u32;
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"RIFF");
        encoded.extend_from_slice(&(36 + data_size).to_le_bytes());
        encoded.extend_from_slice(
            b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data",
        );
        encoded.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            encoded.write_all(&sample.to_le_bytes()).unwrap();
        }
        fs::write(&path, encoded).unwrap();

        let preview = generate(path, MediaFormat::Wav).unwrap();

        assert!(matches!(preview, MediaPreview::Waveform { ref rows } if rows.len() == 9));
        assert!(preview.waveform_lines().iter().any(|row| row.contains('#')));
    }

    #[test]
    fn generates_ffmpeg_audio_and_video_previews() {
        let runner = ToolRunner::default();
        if !runner.ffmpeg_available() || !runner.ffprobe_available() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("audio.ogg");
        let video = directory.path().join("video.mkv");
        runner
            .ffmpeg([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.1",
                audio.to_str().unwrap(),
            ])
            .unwrap();
        runner
            .ffmpeg([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=red:size=16x8:duration=0.1",
                "-frames:v",
                "1",
                video.to_str().unwrap(),
            ])
            .unwrap();

        assert!(matches!(
            generate(audio, MediaFormat::Ogg).unwrap(),
            MediaPreview::Waveform { rows } if !rows.is_empty()
        ));
        assert!(matches!(
            generate(video, MediaFormat::Matroska).unwrap(),
            MediaPreview::Pixels {
                source_width: 16,
                source_height: 8,
                ..
            }
        ));
    }
}
