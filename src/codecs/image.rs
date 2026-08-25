use std::{fs, io::Write, path::Path};

use image::{
    codecs::{avif::AvifEncoder, jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    DynamicImage, ExtendedColorType, ImageBuffer, ImageEncoder, Rgba,
};

use crate::{
    codecs::{jpeg, png, webp},
    filters::{image as image_filters, FilterDomain},
    DatabenderError, MediaFormat, PreparedTransform, Result,
};

type RgbaImage = ImageBuffer<Rgba<u8>, Vec<u8>>;

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
    if format == MediaFormat::WebP && webp::is_animated(&encoded)? {
        return Err(DatabenderError::OutputValidation {
            reason: "animated WebP is planned for v0.5".to_owned(),
        });
    }
    if format == MediaFormat::Avif && is_avif_sequence(&encoded) {
        return Err(DatabenderError::OutputValidation {
            reason: "image-sequence AVIF is planned for v0.5".to_owned(),
        });
    }
    let metadata = ImageMetadata::extract(format, &encoded)?;
    let dimensions = decode_bytes(&encoded, prepared.input())?.dimensions();
    for stage in &prepared.plan().stages {
        match stage.domain {
            FilterDomain::JpegHuffmanTables => {
                for (filter_index, filter) in stage.filters.iter().enumerate() {
                    let crate::FilterSpec::HuffmanGlitch {
                        swaps,
                        intensity,
                        target,
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
                encoded = metadata.inject(&encode(format, &image)?)?;
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
    Jpeg(Vec<jpeg::MetadataSegment>),
    Png(Vec<png::MetadataChunk>),
    WebP(Vec<webp::MetadataChunk>),
    None,
}

impl ImageMetadata {
    fn extract(format: MediaFormat, encoded: &[u8]) -> Result<Self> {
        match format {
            MediaFormat::Jpeg => Ok(Self::Jpeg(jpeg::extract_metadata(encoded)?)),
            MediaFormat::Png => Ok(Self::Png(png::extract_metadata(encoded)?)),
            MediaFormat::WebP => Ok(Self::WebP(webp::extract_metadata(encoded)?)),
            MediaFormat::Avif => Ok(Self::None),
            _ => unreachable!("image format checked before metadata extraction"),
        }
    }

    fn inject(&self, encoded: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Jpeg(metadata) => jpeg::inject_metadata(encoded, metadata),
            Self::Png(metadata) => png::inject_metadata(encoded, metadata),
            Self::WebP(metadata) => webp::inject_metadata(encoded, metadata),
            Self::None => Ok(encoded.to_vec()),
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
    image::load_from_memory(encoded)
        .map(DynamicImage::into_rgba8)
        .map_err(|error| DatabenderError::ImageDecode {
            path: source.to_path_buf(),
            reason: error.to_string(),
        })
}

fn encode(format: MediaFormat, image: &RgbaImage) -> Result<Vec<u8>> {
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
        MediaFormat::Avif => AvifEncoder::new_with_speed_quality(&mut encoded, 6, 90).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        ),
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
        fs::write(path, encode(format, &image).unwrap()).unwrap();
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
}
