use std::io::{Read, Write};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};

use crate::{filters::bytes, DatabenderError, FilterSpec, Result};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const METADATA_KINDS: [[u8; 4]; 10] = [
    *b"cHRM", *b"gAMA", *b"iCCP", *b"sRGB", *b"pHYs", *b"eXIf", *b"tEXt", *b"zTXt", *b"iTXt",
    *b"tIME",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataChunk {
    pub(crate) kind: [u8; 4],
    pub(crate) data: Vec<u8>,
}

pub(crate) fn extract_metadata(input: &[u8]) -> Result<Vec<MetadataChunk>> {
    Ok(parse_chunks(input)?
        .into_iter()
        .filter(|chunk| METADATA_KINDS.contains(&chunk.kind))
        .map(|chunk| MetadataChunk {
            kind: chunk.kind,
            data: chunk.data.to_vec(),
        })
        .collect())
}

pub(crate) fn inject_metadata(encoded: &[u8], metadata: &[MetadataChunk]) -> Result<Vec<u8>> {
    let chunks = parse_chunks(encoded)?;
    let mut output = SIGNATURE.to_vec();
    let mut inserted = false;
    for chunk in chunks {
        if METADATA_KINDS.contains(&chunk.kind) {
            continue;
        }
        write_chunk(&mut output, chunk.kind, chunk.data)?;
        if chunk.kind == *b"IHDR" {
            for metadata_chunk in metadata {
                if !METADATA_KINDS.contains(&metadata_chunk.kind) {
                    return Err(invalid_png("unsupported metadata chunk type"));
                }
                write_chunk(&mut output, metadata_chunk.kind, &metadata_chunk.data)?;
            }
            inserted = true;
        }
    }
    if !inserted {
        return Err(invalid_png("missing IHDR chunk"));
    }
    Ok(output)
}

pub fn mutate_scanlines(input: &[u8], filters: &[FilterSpec], seed: u64) -> Result<Vec<u8>> {
    let chunks = parse_chunks(input)?;
    let header = chunks
        .iter()
        .find(|chunk| chunk.kind == *b"IHDR")
        .ok_or_else(|| invalid_png("missing IHDR chunk"))?;
    let layout = ScanlineLayout::parse(header.data)?;

    let mut compressed = Vec::new();
    for chunk in chunks.iter().filter(|chunk| chunk.kind == *b"IDAT") {
        compressed.extend_from_slice(chunk.data);
    }
    if compressed.is_empty() {
        return Err(invalid_png("missing IDAT data"));
    }

    let mut scanlines = Vec::new();
    ZlibDecoder::new(compressed.as_slice())
        .read_to_end(&mut scanlines)
        .map_err(|error| invalid_png(format!("could not inflate IDAT data: {error}")))?;
    layout.mutate_rows(&mut scanlines, filters, seed)?;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&scanlines)
        .map_err(|error| invalid_png(format!("could not compress IDAT data: {error}")))?;
    let compressed = encoder
        .finish()
        .map_err(|error| invalid_png(format!("could not finish IDAT data: {error}")))?;

    let mut output = Vec::with_capacity(input.len());
    output.extend_from_slice(SIGNATURE);
    let mut wrote_idat = false;
    for chunk in chunks {
        if chunk.kind == *b"IDAT" {
            if !wrote_idat {
                write_chunk(&mut output, *b"IDAT", &compressed)?;
                wrote_idat = true;
            }
        } else {
            write_chunk(&mut output, chunk.kind, chunk.data)?;
        }
    }
    Ok(output)
}

struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn parse_chunks(input: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if !input.starts_with(SIGNATURE) {
        return Err(invalid_png("invalid signature"));
    }

    let mut chunks = Vec::new();
    let mut offset = SIGNATURE.len();
    while offset < input.len() {
        if input.len() - offset < 12 {
            return Err(invalid_png("truncated chunk header"));
        }
        let length = u32::from_be_bytes(
            input[offset..offset + 4]
                .try_into()
                .expect("four-byte length"),
        ) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|value| value.checked_add(length))
            .filter(|end| *end <= input.len())
            .ok_or_else(|| invalid_png("chunk length exceeds file size"))?;
        let kind: [u8; 4] = input[offset + 4..offset + 8]
            .try_into()
            .expect("four-byte chunk type");
        let data = &input[offset + 8..offset + 8 + length];
        let expected_crc = u32::from_be_bytes(
            input[offset + 8 + length..end]
                .try_into()
                .expect("four-byte CRC"),
        );
        let actual_crc = crc(kind, data);
        if actual_crc != expected_crc {
            return Err(invalid_png(format!(
                "invalid {} CRC",
                String::from_utf8_lossy(&kind)
            )));
        }
        chunks.push(Chunk { kind, data });
        offset = end;
        if kind == *b"IEND" {
            if offset != input.len() {
                return Err(invalid_png("data follows IEND chunk"));
            }
            return Ok(chunks);
        }
    }
    Err(invalid_png("missing IEND chunk"))
}

struct ScanlineLayout {
    height: usize,
    row_bytes: usize,
}

impl ScanlineLayout {
    fn parse(header: &[u8]) -> Result<Self> {
        if header.len() != 13 {
            return Err(invalid_png("IHDR must contain 13 bytes"));
        }
        if header[10] != 0 || header[11] != 0 {
            return Err(invalid_png("unsupported PNG compression or filter method"));
        }
        if header[12] != 0 {
            return Err(invalid_png(
                "interlaced PNG payload mutation is not supported",
            ));
        }

        let width = u32::from_be_bytes(header[..4].try_into().expect("four-byte width"));
        let height = u32::from_be_bytes(header[4..8].try_into().expect("four-byte height"));
        let bit_depth = usize::from(header[8]);
        let channels = match header[9] {
            0 => 1,
            2 => 3,
            3 => 1,
            4 => 2,
            6 => 4,
            color_type => return Err(invalid_png(format!("unsupported color type {color_type}"))),
        };
        if width == 0 || height == 0 {
            return Err(invalid_png("image dimensions must be nonzero"));
        }
        let row_bits = (width as usize)
            .checked_mul(channels)
            .and_then(|value| value.checked_mul(bit_depth))
            .ok_or_else(|| invalid_png("scanline size overflow"))?;
        let row_bytes = row_bits
            .checked_add(7)
            .ok_or_else(|| invalid_png("scanline size overflow"))?
            / 8;
        Ok(Self {
            height: height as usize,
            row_bytes,
        })
    }

    fn mutate_rows(&self, scanlines: &mut [u8], filters: &[FilterSpec], seed: u64) -> Result<()> {
        let stride = self.row_bytes + 1;
        let expected = self
            .height
            .checked_mul(stride)
            .ok_or_else(|| invalid_png("scanline data size overflow"))?;
        if scanlines.len() != expected {
            return Err(invalid_png(format!(
                "inflated IDAT has {} bytes; expected {expected}",
                scanlines.len()
            )));
        }
        for (row_index, row) in scanlines.chunks_exact_mut(stride).enumerate() {
            if row[0] > 4 {
                return Err(invalid_png(format!("invalid row filter {}", row[0])));
            }
            bytes::apply(filters, &mut row[1..], seed.wrapping_add(row_index as u64))?;
        }
        Ok(())
    }
}

fn write_chunk(output: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> Result<()> {
    let length = u32::try_from(data.len()).map_err(|_| invalid_png("chunk is too large"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&crc(kind, data).to_be_bytes());
    Ok(())
}

fn crc(kind: [u8; 4], data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&kind);
    hasher.update(data);
    hasher.finalize()
}

fn invalid_png(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid PNG: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, ImageEncoder, Rgba};

    use super::*;

    fn fixture() -> Vec<u8> {
        let image = ImageBuffer::from_fn(8, 4, |x, y| {
            Rgba([(x * 20) as u8, (y * 50) as u8, (x * 10 + y) as u8, 255_u8])
        });
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        encoded
    }

    #[test]
    fn rebuilds_decodable_png_with_valid_chunk_crcs() {
        let output = mutate_scanlines(
            &fixture(),
            &[FilterSpec::ByteNoise { probability: 0.25 }],
            42,
        )
        .unwrap();

        parse_chunks(&output).unwrap();
        let decoded = image::load_from_memory(&output).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 4));
    }

    #[test]
    fn mutation_is_deterministic_and_changes_payload() {
        let input = fixture();
        let filters = [FilterSpec::ByteSwap { count: 10 }];

        let first = mutate_scanlines(&input, &filters, 42).unwrap();
        let second = mutate_scanlines(&input, &filters, 42).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, input);
    }

    #[test]
    fn rejects_corrupt_chunks() {
        let mut input = fixture();
        input[20] ^= 1;
        assert!(mutate_scanlines(&input, &[], 42).is_err());
    }

    #[test]
    fn preserves_ancillary_chunks_around_rebuilt_idat() {
        let input = fixture();
        let chunks = parse_chunks(&input).unwrap();
        let mut with_text = SIGNATURE.to_vec();
        for chunk in chunks {
            write_chunk(&mut with_text, chunk.kind, chunk.data).unwrap();
            if chunk.kind == *b"IHDR" {
                write_chunk(&mut with_text, *b"tEXt", b"Author\0Databender").unwrap();
            }
        }

        let output =
            mutate_scanlines(&with_text, &[FilterSpec::ByteRepeat { count: 4 }], 42).unwrap();
        let output_chunks = parse_chunks(&output).unwrap();

        assert!(output_chunks
            .iter()
            .any(|chunk| chunk.kind == *b"tEXt" && chunk.data == b"Author\0Databender"));
        assert!(image::load_from_memory(&output).is_ok());
    }

    #[test]
    fn transplants_safe_metadata_into_reencoded_png() {
        let metadata = vec![
            MetadataChunk {
                kind: *b"tEXt",
                data: b"Author\0Databender".to_vec(),
            },
            MetadataChunk {
                kind: *b"pHYs",
                data: [0, 0, 11, 19, 0, 0, 11, 19, 1].to_vec(),
            },
        ];

        let output = inject_metadata(&fixture(), &metadata).unwrap();

        assert_eq!(extract_metadata(&output).unwrap(), metadata);
        parse_chunks(&output).unwrap();
        assert!(image::load_from_memory(&output).is_ok());
    }
}
