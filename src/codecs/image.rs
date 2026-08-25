use std::{ffi::OsString, fs, io::Cursor, io::Write, path::Path};

use image::{
    codecs::{avif::AvifEncoder, jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    DynamicImage, ExtendedColorType, ImageBuffer, ImageEncoder, ImageReader, Limits, Rgba,
};

use crate::{
    codecs::{avif, avif_sequence, jpeg, png, webp},
    ffmpeg::ToolRunner,
    filters::{image as image_filters, FilterDomain},
    seed::{derive_seed, SeedIdentity},
    DatabenderError, MediaFormat, PreparedTransform, Result,
};

type RgbaImage = ImageBuffer<Rgba<u8>, Vec<u8>>;

const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_ALLOCATION: u64 = 512 * 1024 * 1024;

pub fn execute(prepared: PreparedTransform) -> Result<std::path::PathBuf> {
    let format = prepared.plan().format;
    if !matches!(
        format,
        MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::WebP | MediaFormat::Avif
    ) {
        return Err(DatabenderError::UnsupportedFormat {
            format: format.to_string(),
        });
    }

    let mut encoded = fs::read(prepared.input()).map_err(|source| DatabenderError::Io {
        path: prepared.input().to_path_buf(),
        source,
    })?;
    if format == MediaFormat::WebP {
        if let Some(animation) = webp::animation_info(&encoded)? {
            return execute_animated_webp(prepared, &encoded, animation);
        }
    }
    if format == MediaFormat::Avif && is_avif_sequence(&encoded) {
        let animation = avif_sequence::parse(&encoded)?;
        return execute_animated_avif(prepared, animation);
    }
    let metadata = ImageMetadata::extract(format, &encoded)?;
    let dimensions = decode_bytes(&encoded, prepared.input())?.dimensions();
    for stage in &prepared.plan().stages {
        prepared.cancellation().check()?;
        match stage.domain {
            FilterDomain::JpegHuffmanTables => {
                for (filter_index, filter) in stage.filters.iter().enumerate() {
                    let crate::FilterSpec::HuffmanGlitch {
                        swaps,
                        intensity,
                        target,
                        engine: _,
                        mode,
                        preserve_size,
                    } = filter
                    else {
                        unreachable!("Huffman stages only contain Huffman filters")
                    };
                    encoded = jpeg::mutate_huffman_tables(
                        &encoded,
                        jpeg::HuffmanGlitchOptions {
                            swaps: *swaps,
                            intensity: *intensity,
                            target: *target,
                            mode: *mode,
                            preserve_size: *preserve_size,
                        },
                        stage.seed.wrapping_add(filter_index as u64),
                    )?;
                }
            }
            FilterDomain::ImagePixels => {
                let mut image = decode_bytes(&encoded, prepared.input())?;
                image_filters::apply(
                    &stage.filters,
                    image.as_mut(),
                    dimensions.0,
                    dimensions.1,
                    stage.seed,
                )?;
                encoded = metadata.inject(&encode(format, &image, Some(&metadata))?)?;
            }
            FilterDomain::EncodedPayload if format == MediaFormat::Png => {
                encoded = png::mutate_scanlines(&encoded, &stage.filters, stage.seed)?;
            }
            FilterDomain::EncodedPayload => {
                let filter = stage
                    .filters
                    .first()
                    .expect("pipeline stages always contain a filter");
                return Err(DatabenderError::IncompatibleFilter {
                    filter: filter.name().to_owned(),
                    format: format.to_string(),
                    reason: "encoded-payload filters currently support PNG only".to_owned(),
                });
            }
            _ => unreachable!("image capability validation excludes other domains"),
        }
    }

    let expected_metadata = metadata.clone();
    prepared.publish_with(
        move |candidate| candidate.write_all(&encoded),
        move |candidate| validate(candidate, format, dimensions, &expected_metadata),
    )
}

fn execute_animated_avif(
    prepared: PreparedTransform,
    animation: avif_sequence::AnimationInfo,
) -> Result<std::path::PathBuf> {
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let raw = workspace.path().join("decoded.rgba");
    let runner = ToolRunner::default().with_cancellation(prepared.cancellation().clone());
    let sequence_streams = runner
        .probe_video_streams(prepared.input())?
        .into_iter()
        .enumerate()
        .filter(|(_, stream)| {
            stream.frame_count == animation.frame_durations.len() as u64
                && stream.properties.width == animation.width
                && stream.properties.height == animation.height
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let expected_streams = if animation.has_alpha { 2 } else { 1 };
    if sequence_streams.len() != expected_streams {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "AVIF sequence expected {expected_streams} synchronized tracks but FFprobe found {}",
                sequence_streams.len()
            ),
        });
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        prepared.input().as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from(format!("0:v:{}", sequence_streams[0])),
        OsString::from("-fps_mode"),
        OsString::from("passthrough"),
        OsString::from("-pix_fmt"),
        OsString::from("rgba"),
        OsString::from("-f"),
        OsString::from("rawvideo"),
        raw.as_os_str().to_owned(),
    ])?;
    let frame_bytes = (animation.width as usize)
        .checked_mul(animation.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "AVIF sequence dimensions overflow the address space".to_owned(),
        })?;
    let expected_bytes = frame_bytes
        .checked_mul(animation.frame_durations.len())
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "AVIF sequence frame buffer exceeds the address space".to_owned(),
        })?;
    let mut frames = fs::read(&raw).map_err(|source| DatabenderError::Io { path: raw, source })?;
    if frames.len() != expected_bytes {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "AVIF sequence declared {} frames but decoded {} complete frames",
                animation.frame_durations.len(),
                frames.len() / frame_bytes
            ),
        });
    }
    if animation.has_alpha {
        let alpha_path = workspace.path().join("decoded-alpha.gray");
        runner.ffmpeg([
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            prepared.input().as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from(format!("0:v:{}", sequence_streams[1])),
            OsString::from("-fps_mode"),
            OsString::from("passthrough"),
            OsString::from("-pix_fmt"),
            OsString::from("gray"),
            OsString::from("-f"),
            OsString::from("rawvideo"),
            alpha_path.as_os_str().to_owned(),
        ])?;
        let alpha = fs::read(&alpha_path).map_err(|source| DatabenderError::Io {
            path: alpha_path,
            source,
        })?;
        if alpha.len().checked_mul(4) != Some(frames.len()) {
            return Err(DatabenderError::OutputValidation {
                reason: "AVIF auxiliary alpha sample count does not match color frames".to_owned(),
            });
        }
        for (pixel, alpha) in frames.chunks_exact_mut(4).zip(alpha) {
            pixel[3] = alpha;
        }
    }
    for (frame_index, frame) in frames.chunks_exact_mut(frame_bytes).enumerate() {
        for stage in &prepared.plan().stages {
            prepared.cancellation().check()?;
            image_filters::apply(
                &stage.filters,
                frame,
                animation.width,
                animation.height,
                derive_seed(stage.seed, SeedIdentity::Frame(frame_index as u64)),
            )?;
        }
        let image = ImageBuffer::from_raw(animation.width, animation.height, frame.to_vec())
            .expect("validated RGBA frame dimensions");
        let path = workspace.path().join(format!("frame-{frame_index:08}.png"));
        fs::write(&path, encode(MediaFormat::Png, &image, None)?)
            .map_err(|source| DatabenderError::Io { path, source })?;
    }

    let animated = workspace.path().join("filtered.avif");
    if animation.has_alpha {
        let repetitions = if animation.loop_count == 0 {
            "infinite".to_owned()
        } else {
            (animation.loop_count - 1).to_string()
        };
        let mut arguments = vec![
            OsString::from("-q"),
            OsString::from("90"),
            OsString::from("--qalpha"),
            OsString::from("100"),
            OsString::from("--timescale"),
            OsString::from(animation.timescale.to_string()),
            OsString::from("--repetition-count"),
            OsString::from(repetitions),
        ];
        for (frame_index, duration) in animation.frame_durations.iter().enumerate() {
            arguments.extend([
                OsString::from("--duration"),
                OsString::from(duration.to_string()),
                workspace
                    .path()
                    .join(format!("frame-{frame_index:08}.png"))
                    .into_os_string(),
            ]);
        }
        arguments.push(animated.as_os_str().to_owned());
        runner.avifenc(arguments)?;
    } else {
        let output_timescale = animation.timescale.checked_mul(2).ok_or_else(|| {
            DatabenderError::OutputValidation {
                reason: "AVIF sequence timescale is too large".to_owned(),
            }
        })?;
        let mut arguments = vec![
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-y"),
        ];
        for frame_index in 0..animation.frame_durations.len() {
            arguments.extend([
                OsString::from("-framerate"),
                OsString::from(output_timescale.to_string()),
                OsString::from("-i"),
                workspace
                    .path()
                    .join(format!("frame-{frame_index:08}.png"))
                    .into_os_string(),
            ]);
        }
        let mut timestamp = 0_u64;
        let mut graph = String::new();
        for (index, duration) in animation.frame_durations.iter().enumerate() {
            graph.push_str(&format!(
                "[{index}:v]settb=1/{output_timescale},setpts={}[f{index}];",
                timestamp * 2
            ));
            timestamp += u64::from(*duration);
        }
        for index in 0..animation.frame_durations.len() {
            graph.push_str(&format!("[f{index}]"));
        }
        graph.push_str(&format!("interleave=n={}", animation.frame_durations.len()));
        arguments.extend([
            OsString::from("-filter_complex"),
            OsString::from(graph),
            OsString::from("-fps_mode"),
            OsString::from("vfr"),
            OsString::from("-enc_time_base"),
            OsString::from(format!("1/{output_timescale}")),
            OsString::from("-loop"),
            OsString::from(animation.loop_count.to_string()),
            OsString::from("-f"),
            OsString::from("avif"),
            animated.as_os_str().to_owned(),
        ]);
        runner.ffmpeg(arguments)?;
    }
    let mut filtered = fs::read(&animated).map_err(|source| DatabenderError::Io {
        path: animated,
        source,
    })?;
    if !animation.has_alpha {
        avif_sequence::repair_timing(&mut filtered, &animation)?;
    }
    let expected = animation.clone();
    prepared.publish_with(
        move |candidate| candidate.write_all(&filtered),
        move |candidate| validate_animated_avif(candidate, &expected, &runner),
    )
}

fn validate_animated_avif(
    path: &Path,
    expected: &avif_sequence::AnimationInfo,
    runner: &ToolRunner,
) -> Result<()> {
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !avif_sequence::parse(&encoded)?.equivalent_to(expected) {
        return Err(DatabenderError::OutputValidation {
            reason: "AVIF sequence timing, order, dimensions, or loop count changed".to_owned(),
        });
    }
    let sequence_streams = runner
        .probe_video_streams(path)?
        .into_iter()
        .enumerate()
        .filter(|(_, stream)| {
            stream.frame_count == expected.frame_durations.len() as u64
                && stream.properties.width == expected.width
                && stream.properties.height == expected.height
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if sequence_streams.len() != if expected.has_alpha { 2 } else { 1 } {
        return Err(DatabenderError::OutputValidation {
            reason: "AVIF sequence track topology changed".to_owned(),
        });
    }
    let mut arguments = vec![
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        path.as_os_str().to_owned(),
    ];
    for stream in sequence_streams {
        arguments.extend([
            OsString::from("-map"),
            OsString::from(format!("0:v:{stream}")),
        ]);
    }
    arguments.extend([
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ]);
    runner.ffmpeg(arguments)?;
    Ok(())
}

fn execute_animated_webp(
    prepared: PreparedTransform,
    encoded: &[u8],
    animation: webp::AnimationInfo,
) -> Result<std::path::PathBuf> {
    let metadata = ImageMetadata::extract(MediaFormat::WebP, encoded)?;
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: std::env::temp_dir(),
        source,
    })?;
    let raw = workspace.path().join("decoded.rgba");
    let runner = ToolRunner::default().with_cancellation(prepared.cancellation().clone());
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        prepared.input().as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-fps_mode"),
        OsString::from("passthrough"),
        OsString::from("-pix_fmt"),
        OsString::from("rgba"),
        OsString::from("-f"),
        OsString::from("rawvideo"),
        raw.as_os_str().to_owned(),
    ])?;
    let frame_bytes = (animation.width as usize)
        .checked_mul(animation.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "animated WebP dimensions overflow the address space".to_owned(),
        })?;
    let expected_bytes = frame_bytes
        .checked_mul(animation.frame_durations_ms.len())
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "animated WebP frame buffer exceeds the address space".to_owned(),
        })?;
    let mut frames = fs::read(&raw).map_err(|source| DatabenderError::Io {
        path: raw.clone(),
        source,
    })?;
    if frames.len() != expected_bytes {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "animated WebP declared {} frames but decoded {} complete frames",
                animation.frame_durations_ms.len(),
                frames.len() / frame_bytes
            ),
        });
    }
    for (frame_index, frame) in frames.chunks_exact_mut(frame_bytes).enumerate() {
        for stage in &prepared.plan().stages {
            prepared.cancellation().check()?;
            image_filters::apply(
                &stage.filters,
                frame,
                animation.width,
                animation.height,
                derive_seed(stage.seed, SeedIdentity::Frame(frame_index as u64)),
            )?;
        }
        let image = ImageBuffer::from_raw(animation.width, animation.height, frame.to_vec())
            .expect("validated RGBA frame dimensions");
        let path = workspace.path().join(format!("frame-{frame_index:08}.png"));
        fs::write(&path, encode(MediaFormat::Png, &image, None)?).map_err(|source| {
            DatabenderError::Io {
                path: path.clone(),
                source,
            }
        })?;
    }

    let animated = workspace.path().join("filtered.webp");
    let mut arguments = vec![
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
    ];
    for (frame_index, duration) in animation.frame_durations_ms.iter().enumerate() {
        arguments.extend([
            OsString::from("-loop"),
            OsString::from("1"),
            OsString::from("-framerate"),
            OsString::from("1000"),
            OsString::from("-t"),
            OsString::from(format!("{}.{:03}", duration / 1000, duration % 1000)),
            OsString::from("-i"),
            workspace
                .path()
                .join(format!("frame-{frame_index:08}.png"))
                .into_os_string(),
        ]);
    }
    let inputs = (0..animation.frame_durations_ms.len())
        .map(|index| format!("[{index}:v]"))
        .collect::<String>();
    arguments.extend([
        OsString::from("-filter_complex"),
        OsString::from(format!(
            "{inputs}concat=n={}:v=1:a=0",
            animation.frame_durations_ms.len()
        )),
        OsString::from("-fps_mode"),
        OsString::from("vfr"),
        OsString::from("-lossless"),
        OsString::from("1"),
        OsString::from("-loop"),
        OsString::from(animation.loop_count.to_string()),
        OsString::from("-f"),
        OsString::from("webp"),
        animated.as_os_str().to_owned(),
    ]);
    runner.ffmpeg(arguments)?;
    let filtered = fs::read(&animated).map_err(|source| DatabenderError::Io {
        path: animated,
        source,
    })?;
    let filtered = metadata.inject(&filtered)?;
    let expected_metadata = metadata.clone();
    let expected_animation = animation.clone();
    prepared.publish_with(
        move |candidate| candidate.write_all(&filtered),
        move |candidate| {
            validate_animated_webp(candidate, &expected_animation, &expected_metadata, &runner)
        },
    )
}

fn validate_animated_webp(
    path: &Path,
    expected: &webp::AnimationInfo,
    expected_metadata: &ImageMetadata,
    runner: &ToolRunner,
) -> Result<()> {
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if webp::animation_info(&encoded)?.as_ref() != Some(expected) {
        return Err(DatabenderError::OutputValidation {
            reason: "animated WebP frame timing, order, dimensions, or loop count changed"
                .to_owned(),
        });
    }
    if ImageMetadata::extract(MediaFormat::WebP, &encoded)? != *expected_metadata {
        return Err(DatabenderError::OutputValidation {
            reason: "supported image metadata changed during transformation".to_owned(),
        });
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        path.as_os_str().to_owned(),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ])?;
    Ok(())
}

fn is_avif_sequence(encoded: &[u8]) -> bool {
    if encoded.len() < 16 || encoded.get(4..8) != Some(b"ftyp") {
        return false;
    }
    let box_size = u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize;
    let end = box_size.min(encoded.len());
    (8..end)
        .step_by(4)
        .any(|offset| encoded.get(offset..offset + 4) == Some(b"avis"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ImageMetadata {
    Avif(avif::Metadata),
    Jpeg(Vec<jpeg::MetadataSegment>),
    Png(Vec<png::MetadataChunk>),
    WebP(Vec<webp::MetadataChunk>),
}

impl ImageMetadata {
    fn extract(format: MediaFormat, encoded: &[u8]) -> Result<Self> {
        match format {
            MediaFormat::Jpeg => Ok(Self::Jpeg(jpeg::extract_metadata(encoded)?)),
            MediaFormat::Png => Ok(Self::Png(png::extract_metadata(encoded)?)),
            MediaFormat::WebP => Ok(Self::WebP(webp::extract_metadata(encoded)?)),
            MediaFormat::Avif => Ok(Self::Avif(avif::extract_metadata(encoded)?)),
            _ => unreachable!("image format checked before metadata extraction"),
        }
    }

    fn inject(&self, encoded: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Avif(_) => Ok(encoded.to_vec()),
            Self::Jpeg(metadata) => jpeg::inject_metadata(encoded, metadata),
            Self::Png(metadata) => png::inject_metadata(encoded, metadata),
            Self::WebP(metadata) => webp::inject_metadata(encoded, metadata),
        }
    }
}

fn decode(path: &Path) -> Result<RgbaImage> {
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    decode_bytes(&encoded, path)
}

fn decode_bytes(encoded: &[u8], source: &Path) -> Result<RgbaImage> {
    let decode_error = |error: &dyn std::fmt::Display| DatabenderError::ImageDecode {
        path: source.to_path_buf(),
        reason: error.to_string(),
    };
    let mut reader = ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .map_err(|error| decode_error(&error))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION);
    reader.limits(limits);
    reader
        .decode()
        .map(DynamicImage::into_rgba8)
        .map_err(|error| decode_error(&error))
}

fn encode(
    format: MediaFormat,
    image: &RgbaImage,
    metadata: Option<&ImageMetadata>,
) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    let result = match format {
        MediaFormat::Png => PngEncoder::new(&mut encoded).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        ),
        MediaFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgba8(image.clone()).into_rgb8();
            JpegEncoder::new_with_quality(&mut encoded, 90).encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
        }
        MediaFormat::WebP => WebPEncoder::new_lossless(&mut encoded).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        ),
        MediaFormat::Avif => {
            let mut encoder = AvifEncoder::new_with_speed_quality(&mut encoded, 6, 90);
            if let Some(ImageMetadata::Avif(avif::Metadata { exif: Some(exif) })) = metadata {
                encoder.set_exif_metadata(exif.clone()).map_err(|error| {
                    DatabenderError::ImageEncode {
                        format: format.to_string(),
                        reason: error.to_string(),
                    }
                })?;
            }
            encoder.write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
        }
        _ => unreachable!("format checked before encoding"),
    };
    result.map_err(|error| DatabenderError::ImageEncode {
        format: format.to_string(),
        reason: error.to_string(),
    })?;
    Ok(encoded)
}

fn validate(
    path: &Path,
    format: MediaFormat,
    dimensions: (u32, u32),
    expected_metadata: &ImageMetadata,
) -> Result<()> {
    let detected = MediaFormat::detect(path)?;
    if detected != format {
        return Err(DatabenderError::OutputValidation {
            reason: format!("encoded {detected} instead of {format}"),
        });
    }
    let decoded = decode(path)?;
    if decoded.dimensions() != dimensions {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "dimensions changed from {}x{} to {}x{}",
                dimensions.0,
                dimensions.1,
                decoded.width(),
                decoded.height()
            ),
        });
    }
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let actual_metadata = ImageMetadata::extract(format, &encoded)?;
    if actual_metadata != *expected_metadata {
        return Err(DatabenderError::OutputValidation {
            reason: "supported image metadata changed during transformation".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{FilterSpec, TransformRequest};

    fn write_fixture(path: &Path, format: MediaFormat) {
        let image = ImageBuffer::from_fn(4, 2, |x, y| {
            Rgba([
                (x * 50) as u8,
                (y * 100) as u8,
                (x * 30 + y * 20) as u8,
                (64 + x * 40 + y * 16) as u8,
            ])
        });
        fs::write(path, encode(format, &image, None).unwrap()).unwrap();
    }

    fn transform(
        format: MediaFormat,
        filters: Vec<FilterSpec>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.media");
        let output = directory.path().join("output.media");
        write_fixture(&input, format);

        let prepared = TransformRequest::new(input, &output, filters, 42)
            .prepare()
            .unwrap();
        execute(prepared).unwrap();
        (directory, output)
    }

    #[test]
    fn transforms_png_and_preserves_dimensions() {
        let (_directory, output) = transform(
            MediaFormat::Png,
            vec![FilterSpec::ChannelShift { pixels: 1 }],
        );

        assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Png);
        assert_eq!(decode(&output).unwrap().dimensions(), (4, 2));
    }

    #[test]
    fn transforms_jpeg_and_preserves_dimensions() {
        let (_directory, output) = transform(
            MediaFormat::Jpeg,
            vec![FilterSpec::PixelSort { threshold: 40 }],
        );

        assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Jpeg);
        assert_eq!(decode(&output).unwrap().dimensions(), (4, 2));
    }

    #[test]
    fn transforms_webp_and_preserves_dimensions_and_alpha() {
        let (_directory, output) = transform(MediaFormat::WebP, vec![FilterSpec::Invert]);

        assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::WebP);
        let decoded = decode(&output).unwrap();
        assert_eq!(decoded.dimensions(), (4, 2));
        let alpha = decoded.pixels().map(|pixel| pixel[3]).collect::<Vec<_>>();
        assert!(alpha.iter().any(|value| *value < 255));
        assert!(alpha.iter().max().unwrap() > alpha.iter().min().unwrap());
    }

    #[test]
    fn transforms_avif_and_preserves_dimensions_and_alpha() {
        let (_directory, output) = transform(MediaFormat::Avif, vec![FilterSpec::Invert]);

        assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Avif);
        let decoded = decode(&output).unwrap();
        assert_eq!(decoded.dimensions(), (4, 2));
        let alpha = decoded.pixels().map(|pixel| pixel[3]).collect::<Vec<_>>();
        assert!(alpha.iter().any(|value| *value < 255));
        assert!(alpha.iter().max().unwrap() > alpha.iter().min().unwrap());
    }

    #[test]
    fn rejects_oversized_image_dimensions_before_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.png");
        let image = ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 255]));
        let mut encoded = encode(MediaFormat::Png, &image, None).unwrap();
        encoded[16..20].copy_from_slice(&(MAX_IMAGE_DIMENSION + 1).to_be_bytes());
        let checksum = crc32fast::hash(&encoded[12..29]).to_be_bytes();
        encoded[29..33].copy_from_slice(&checksum);

        let error = decode_bytes(&encoded, &path).unwrap_err();

        let DatabenderError::ImageDecode { reason, .. } = error else {
            panic!("expected image decode error");
        };
        assert!(reason.to_ascii_lowercase().contains("limit"), "{reason}");
    }

    #[test]
    fn preserves_avif_exif_across_pixel_filters() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.avif");
        let output = directory.path().join("output.avif");
        let image = ImageBuffer::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
        let exif = b"II\x2a\0\x08\0\0\0\0\0\0\0".to_vec();
        let metadata = ImageMetadata::Avif(avif::Metadata {
            exif: Some(exif.clone()),
        });
        fs::write(
            &input,
            encode(MediaFormat::Avif, &image, Some(&metadata)).unwrap(),
        )
        .unwrap();

        let prepared = TransformRequest::new(input, &output, vec![FilterSpec::Invert], 42)
            .prepare()
            .unwrap();
        execute(prepared).unwrap();

        let mut expected = 0_u32.to_be_bytes().to_vec();
        expected.extend_from_slice(&exif);
        assert_eq!(
            avif::extract_metadata(&fs::read(output).unwrap()).unwrap(),
            avif::Metadata {
                exif: Some(expected)
            }
        );
    }

    #[test]
    fn detects_avif_sequence_compatible_brand() {
        assert!(is_avif_sequence(b"\0\0\0\x18ftypavif\0\0\0\0avis"));
        assert!(!is_avif_sequence(b"\0\0\0\x18ftypavif\0\0\0\0mif1"));
    }

    #[test]
    fn transforms_jpeg_huffman_tables() {
        let (_directory, output) = transform(
            MediaFormat::Jpeg,
            vec![FilterSpec::parse("huffman-glitch").unwrap()],
        );

        assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Jpeg);
        assert_eq!(decode(&output).unwrap().dimensions(), (4, 2));
    }

    #[test]
    fn native_png_output_is_deterministic() {
        let (_first_directory, first) = transform(
            MediaFormat::Png,
            vec![FilterSpec::ScanlineDisplacement { max_shift: 2 }],
        );
        let (_second_directory, second) = transform(
            MediaFormat::Png,
            vec![FilterSpec::ScanlineDisplacement { max_shift: 2 }],
        );

        assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
    }

    #[test]
    fn transforms_png_encoded_payload_stages() {
        let (_directory, output) = transform(
            MediaFormat::Png,
            vec![
                FilterSpec::ByteNoise { probability: 0.2 },
                FilterSpec::ByteSwap { count: 4 },
            ],
        );

        assert_eq!(decode(&output).unwrap().dimensions(), (4, 2));
    }

    #[test]
    fn preserves_mixed_png_stage_order() {
        let (_directory, output) = transform(
            MediaFormat::Png,
            vec![
                FilterSpec::ByteRepeat { count: 2 },
                FilterSpec::ChannelShift { pixels: 1 },
                FilterSpec::ByteDrop { count: 2 },
            ],
        );

        assert_eq!(decode(&output).unwrap().dimensions(), (4, 2));
    }

    #[test]
    fn rejects_jpeg_encoded_payload_stages_without_output() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.jpg");
        let output = directory.path().join("output.jpg");
        write_fixture(&input, MediaFormat::Jpeg);
        let error = TransformRequest::new(
            input,
            &output,
            vec![FilterSpec::ByteNoise { probability: 0.1 }],
            42,
        )
        .prepare()
        .unwrap_err();

        assert!(matches!(error, DatabenderError::IncompatibleFilter { .. }));
        assert!(!output.exists());
    }

    #[test]
    fn preserves_jpeg_metadata_across_pixel_filters() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.jpg");
        let output = directory.path().join("output.jpg");
        write_fixture(&input, MediaFormat::Jpeg);
        let metadata = vec![
            jpeg::MetadataSegment {
                marker: 0xe1,
                data: b"Exif\0\0fixture".to_vec(),
            },
            jpeg::MetadataSegment {
                marker: 0xfe,
                data: b"Databender".to_vec(),
            },
        ];
        let with_metadata = jpeg::inject_metadata(&fs::read(&input).unwrap(), &metadata).unwrap();
        fs::write(&input, with_metadata).unwrap();

        let prepared = TransformRequest::new(
            input,
            &output,
            vec![FilterSpec::ChannelShift { pixels: 1 }],
            42,
        )
        .prepare()
        .unwrap();
        execute(prepared).unwrap();

        assert_eq!(
            jpeg::extract_metadata(&fs::read(output).unwrap()).unwrap(),
            metadata
        );
    }

    #[test]
    fn preserves_png_metadata_across_mixed_filters() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.png");
        let output = directory.path().join("output.png");
        write_fixture(&input, MediaFormat::Png);
        let metadata = vec![png::MetadataChunk {
            kind: *b"tEXt",
            data: b"Author\0Databender".to_vec(),
        }];
        let with_metadata = png::inject_metadata(&fs::read(&input).unwrap(), &metadata).unwrap();
        fs::write(&input, with_metadata).unwrap();

        let prepared = TransformRequest::new(
            input,
            &output,
            vec![
                FilterSpec::ByteSwap { count: 2 },
                FilterSpec::PixelSort { threshold: 40 },
                FilterSpec::ByteNoise { probability: 0.1 },
            ],
            42,
        )
        .prepare()
        .unwrap();
        execute(prepared).unwrap();

        assert_eq!(
            png::extract_metadata(&fs::read(output).unwrap()).unwrap(),
            metadata
        );
    }

    #[test]
    fn preserves_webp_metadata_across_pixel_filters() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.webp");
        let output = directory.path().join("output.webp");
        write_fixture(&input, MediaFormat::WebP);
        let metadata = vec![
            webp::MetadataChunk {
                kind: *b"ICCP",
                data: b"fixture profile".to_vec(),
            },
            webp::MetadataChunk {
                kind: *b"EXIF",
                data: b"Exif\0\0fixture".to_vec(),
            },
            webp::MetadataChunk {
                kind: *b"XMP ",
                data: b"<x:xmpmeta>fixture</x:xmpmeta>".to_vec(),
            },
        ];
        let with_metadata = webp::inject_metadata(&fs::read(&input).unwrap(), &metadata).unwrap();
        fs::write(&input, with_metadata).unwrap();

        let prepared = TransformRequest::new(input, &output, vec![FilterSpec::Invert], 42)
            .prepare()
            .unwrap();
        execute(prepared).unwrap();

        assert_eq!(
            webp::extract_metadata(&fs::read(output).unwrap()).unwrap(),
            metadata
        );
    }
}
