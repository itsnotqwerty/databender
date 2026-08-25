use crate::{DatabenderError, Result};

const METADATA_KINDS: [[u8; 4]; 3] = [*b"ICCP", *b"EXIF", *b"XMP "];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataChunk {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
}

pub(crate) fn extract_metadata(encoded: &[u8]) -> Result<Vec<MetadataChunk>> {
    Ok(parse_chunks(encoded)?
        .into_iter()
        .filter(|chunk| METADATA_KINDS.contains(&chunk.kind))
        .map(|chunk| MetadataChunk {
            kind: chunk.kind,
            data: chunk.data.to_vec(),
        })
        .collect())
}

pub(crate) fn inject_metadata(encoded: &[u8], metadata: &[MetadataChunk]) -> Result<Vec<u8>> {
    if metadata.is_empty() {
        return Ok(encoded.to_vec());
    }
    let chunks = parse_chunks(encoded)?;
    let (width, height, alpha) = canvas(&chunks)?;
    let mut body = b"WEBP".to_vec();
    let mut flags = if alpha { 0x10 } else { 0 };
    for chunk in metadata {
        flags |= match &chunk.kind {
            b"ICCP" => 0x20,
            b"EXIF" => 0x08,
            b"XMP " => 0x04,
            _ => return Err(invalid_webp("unsupported metadata chunk")),
        };
    }
    let mut extended = vec![flags, 0, 0, 0];
    write_u24(&mut extended, width - 1);
    write_u24(&mut extended, height - 1);
    write_chunk(&mut body, *b"VP8X", &extended)?;
    for chunk in metadata.iter().filter(|chunk| chunk.kind == *b"ICCP") {
        write_chunk(&mut body, chunk.kind, &chunk.data)?;
    }
    for chunk in &chunks {
        if chunk.kind != *b"VP8X" && !METADATA_KINDS.contains(&chunk.kind) {
            write_chunk(&mut body, chunk.kind, chunk.data)?;
        }
    }
    for chunk in metadata.iter().filter(|chunk| chunk.kind != *b"ICCP") {
        write_chunk(&mut body, chunk.kind, &chunk.data)?;
    }
    let riff_size = u32::try_from(body.len()).map_err(|_| invalid_webp("file is too large"))?;
    let mut output = b"RIFF".to_vec();
    output.extend_from_slice(&riff_size.to_le_bytes());
    output.extend_from_slice(&body);
    Ok(output)
}

pub(crate) fn is_animated(encoded: &[u8]) -> Result<bool> {
    let chunks = parse_chunks(encoded)?;
    Ok(chunks.iter().any(|chunk| {
        matches!(&chunk.kind, b"ANIM" | b"ANMF")
            || (chunk.kind == *b"VP8X" && chunk.data.first().is_some_and(|flags| flags & 0x02 != 0))
    }))
}

struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn parse_chunks(encoded: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if encoded.len() < 12 || &encoded[..4] != b"RIFF" || &encoded[8..12] != b"WEBP" {
        return Err(invalid_webp("missing RIFF/WEBP header"));
    }
    let declared = u32::from_le_bytes(encoded[4..8].try_into().unwrap()) as usize;
    if declared.checked_add(8) != Some(encoded.len()) {
        return Err(invalid_webp("RIFF size does not match file length"));
    }
    let mut chunks = Vec::new();
    let mut offset = 12;
    while offset < encoded.len() {
        if encoded.len() - offset < 8 {
            return Err(invalid_webp("truncated chunk header"));
        }
        let kind = encoded[offset..offset + 4].try_into().unwrap();
        let size = u32::from_le_bytes(encoded[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| invalid_webp("chunk exceeds RIFF bounds"))?;
        chunks.push(Chunk {
            kind,
            data: &encoded[start..end],
        });
        offset = end
            .checked_add(size & 1)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| invalid_webp("missing chunk padding"))?;
    }
    Ok(chunks)
}

fn canvas(chunks: &[Chunk<'_>]) -> Result<(u32, u32, bool)> {
    for chunk in chunks {
        if chunk.kind == *b"VP8X" && chunk.data.len() == 10 {
            return Ok((
                read_u24(&chunk.data[4..7]) + 1,
                read_u24(&chunk.data[7..10]) + 1,
                chunk.data[0] & 0x10 != 0,
            ));
        }
        if chunk.kind == *b"VP8L" && chunk.data.len() >= 5 && chunk.data[0] == 0x2f {
            let bits = u32::from_le_bytes(chunk.data[1..5].try_into().unwrap());
            return Ok((
                (bits & 0x3fff) + 1,
                ((bits >> 14) & 0x3fff) + 1,
                bits & (1 << 28) != 0,
            ));
        }
    }
    Err(invalid_webp("could not determine canvas dimensions"))
}

fn read_u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

fn write_u24(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes()[..3]);
}

fn write_chunk(output: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> Result<()> {
    let size = u32::try_from(data.len()).map_err(|_| invalid_webp("chunk is too large"))?;
    output.extend_from_slice(&kind);
    output.extend_from_slice(&size.to_le_bytes());
    output.extend_from_slice(data);
    if !data.len().is_multiple_of(2) {
        output.push(0);
    }
    Ok(())
}

fn invalid_webp(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid WebP: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use image::{codecs::webp::WebPEncoder, ExtendedColorType, ImageEncoder};

    use super::*;

    fn fixture() -> Vec<u8> {
        let mut encoded = Vec::new();
        WebPEncoder::new_lossless(&mut encoded)
            .write_image(&[10, 20, 30, 128], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        encoded
    }

    #[test]
    fn injects_and_extracts_metadata_without_losing_alpha() {
        let metadata = vec![
            MetadataChunk {
                kind: *b"ICCP",
                data: vec![1, 2, 3],
            },
            MetadataChunk {
                kind: *b"EXIF",
                data: vec![4, 5],
            },
        ];

        let encoded = inject_metadata(&fixture(), &metadata).unwrap();

        assert_eq!(extract_metadata(&encoded).unwrap(), metadata);
        assert!(!is_animated(&encoded).unwrap());
        assert_eq!(
            canvas(&parse_chunks(&encoded).unwrap()).unwrap(),
            (1, 1, true)
        );
    }

    #[test]
    fn rejects_truncated_chunks() {
        let mut encoded = fixture();
        encoded.pop();
        assert!(extract_metadata(&encoded).is_err());
    }

    #[test]
    fn detects_animation_chunks() {
        let mut encoded = fixture();
        let mut body = encoded.split_off(12);
        encoded.truncate(12);
        write_chunk(&mut encoded, *b"ANIM", &[0; 6]).unwrap();
        encoded.append(&mut body);
        let riff_size = u32::try_from(encoded.len() - 8).unwrap();
        encoded[4..8].copy_from_slice(&riff_size.to_le_bytes());

        assert!(is_animated(&encoded).unwrap());
    }
}
