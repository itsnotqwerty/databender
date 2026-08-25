use crate::{DatabenderError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Metadata {
    pub exif: Option<Vec<u8>>,
}

pub(crate) fn extract_metadata(encoded: &[u8]) -> Result<Metadata> {
    let meta = find_box(encoded, 0, encoded.len(), *b"meta")?
        .ok_or_else(|| invalid_avif("missing meta box"))?;
    let children_start = meta
        .data
        .checked_add(4)
        .filter(|start| *start <= meta.end)
        .ok_or_else(|| invalid_avif("truncated meta full-box header"))?;
    let iinf = find_box(encoded, children_start, meta.end, *b"iinf")?
        .ok_or_else(|| invalid_avif("missing iinf box"))?;
    let iloc = find_box(encoded, children_start, meta.end, *b"iloc")?
        .ok_or_else(|| invalid_avif("missing iloc box"))?;
    let exif = find_exif_item(encoded, iinf)?
        .map(|item_id| read_item(encoded, iloc, item_id))
        .transpose()?;
    Ok(Metadata { exif })
}

#[derive(Clone, Copy)]
struct BoxRange {
    data: usize,
    end: usize,
}

fn boxes(encoded: &[u8], mut offset: usize, end: usize) -> Result<Vec<BoxRange>> {
    let mut result = Vec::new();
    while offset < end {
        if end - offset < 8 {
            return Err(invalid_avif("truncated box header"));
        }
        let size32 = read_u32(encoded, offset)?;
        let (header, size) = if size32 == 1 {
            (
                16,
                usize::try_from(read_u64(encoded, offset + 8)?)
                    .map_err(|_| invalid_avif("box is too large"))?,
            )
        } else if size32 == 0 {
            (8, end - offset)
        } else {
            (8, size32 as usize)
        };
        let box_end = offset
            .checked_add(size)
            .filter(|box_end| size >= header && *box_end <= end)
            .ok_or_else(|| invalid_avif("box exceeds parent bounds"))?;
        result.push(BoxRange {
            data: offset + header,
            end: box_end,
        });
        offset = box_end;
    }
    Ok(result)
}

fn find_box(encoded: &[u8], start: usize, end: usize, kind: [u8; 4]) -> Result<Option<BoxRange>> {
    Ok(boxes(encoded, start, end)?
        .into_iter()
        .find(|range| encoded[range.data - 4..range.data] == kind))
}

fn find_exif_item(encoded: &[u8], iinf: BoxRange) -> Result<Option<u32>> {
    let version = read_u8(encoded, iinf.data)?;
    let (count, children_start) = match version {
        0 => (u32::from(read_u16(encoded, iinf.data + 4)?), iinf.data + 6),
        1 => (read_u32(encoded, iinf.data + 4)?, iinf.data + 8),
        _ => return Err(invalid_avif("unsupported iinf version")),
    };
    let entries = boxes(encoded, children_start, iinf.end)?;
    if entries.len() != count as usize {
        return Err(invalid_avif("iinf entry count does not match its children"));
    }
    let mut exif_id = None;
    for entry in entries {
        if encoded[entry.data - 4..entry.data] != *b"infe" {
            return Err(invalid_avif("iinf contains a non-infe child"));
        }
        let version = read_u8(encoded, entry.data)?;
        let (item_id, type_offset) = match version {
            2 => (
                u32::from(read_u16(encoded, entry.data + 4)?),
                entry.data + 8,
            ),
            3 => (read_u32(encoded, entry.data + 4)?, entry.data + 10),
            _ => return Err(invalid_avif("unsupported infe version")),
        };
        if read_fourcc(encoded, type_offset)? == *b"Exif" && exif_id.replace(item_id).is_some() {
            return Err(invalid_avif("multiple Exif items are unsupported"));
        }
    }
    Ok(exif_id)
}

fn read_item(encoded: &[u8], iloc: BoxRange, expected_id: u32) -> Result<Vec<u8>> {
    let version = read_u8(encoded, iloc.data)?;
    if version > 2 {
        return Err(invalid_avif("unsupported iloc version"));
    }
    let sizes = read_u16(encoded, iloc.data + 4)?;
    let offset_size = usize::from((sizes >> 12) & 0xf);
    let length_size = usize::from((sizes >> 8) & 0xf);
    let base_offset_size = usize::from((sizes >> 4) & 0xf);
    let index_size = if version == 0 {
        0
    } else {
        usize::from(sizes & 0xf)
    };
    let mut cursor = iloc.data + 6;
    let item_count = if version < 2 {
        let count = u32::from(read_u16(encoded, cursor)?);
        cursor += 2;
        count
    } else {
        let count = read_u32(encoded, cursor)?;
        cursor += 4;
        count
    };
    for _ in 0..item_count {
        let item_id = if version < 2 {
            let id = u32::from(read_u16(encoded, cursor)?);
            cursor += 2;
            id
        } else {
            let id = read_u32(encoded, cursor)?;
            cursor += 4;
            id
        };
        let construction_method = if version == 0 {
            0
        } else {
            let value = read_u16(encoded, cursor)? & 0xf;
            cursor += 2;
            value
        };
        let data_reference = read_u16(encoded, cursor)?;
        cursor += 2;
        let base_offset = read_sized(encoded, &mut cursor, base_offset_size)?;
        let extent_count = read_u16(encoded, cursor)?;
        cursor += 2;
        let mut item = Vec::new();
        for _ in 0..extent_count {
            let _index = read_sized(encoded, &mut cursor, index_size)?;
            let extent_offset = read_sized(encoded, &mut cursor, offset_size)?;
            let extent_length = read_sized(encoded, &mut cursor, length_size)?;
            if item_id == expected_id {
                if construction_method != 0 || data_reference != 0 {
                    return Err(invalid_avif("Exif item must use local file extents"));
                }
                let start = base_offset
                    .checked_add(extent_offset)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| invalid_avif("Exif extent offset overflows"))?;
                let length = usize::try_from(extent_length)
                    .map_err(|_| invalid_avif("Exif extent is too large"))?;
                let end = start
                    .checked_add(length)
                    .filter(|end| *end <= encoded.len())
                    .ok_or_else(|| invalid_avif("Exif extent exceeds file bounds"))?;
                item.extend_from_slice(&encoded[start..end]);
            }
        }
        if item_id == expected_id {
            if item.is_empty() {
                return Err(invalid_avif("Exif item contains no data"));
            }
            return Ok(item);
        }
    }
    Err(invalid_avif("Exif item is missing from iloc"))
}

fn read_sized(encoded: &[u8], cursor: &mut usize, size: usize) -> Result<u64> {
    if !matches!(size, 0 | 4 | 8) {
        return Err(invalid_avif("unsupported iloc integer size"));
    }
    let mut value = 0_u64;
    for _ in 0..size {
        value = (value << 8) | u64::from(read_u8(encoded, *cursor)?);
        *cursor += 1;
    }
    Ok(value)
}

fn read_u8(encoded: &[u8], offset: usize) -> Result<u8> {
    encoded
        .get(offset)
        .copied()
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn read_u16(encoded: &[u8], offset: usize) -> Result<u16> {
    encoded
        .get(offset..offset + 2)
        .map(|bytes| u16::from_be_bytes(bytes.try_into().unwrap()))
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn read_u32(encoded: &[u8], offset: usize) -> Result<u32> {
    encoded
        .get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()))
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn read_u64(encoded: &[u8], offset: usize) -> Result<u64> {
    encoded
        .get(offset..offset + 8)
        .map(|bytes| u64::from_be_bytes(bytes.try_into().unwrap()))
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn read_fourcc(encoded: &[u8], offset: usize) -> Result<[u8; 4]> {
    encoded
        .get(offset..offset + 4)
        .map(|bytes| bytes.try_into().unwrap())
        .ok_or_else(|| invalid_avif("truncated item type"))
}

fn invalid_avif(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid AVIF: {}", reason.into()),
    }
}
