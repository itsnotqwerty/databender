use crate::{DatabenderError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Metadata {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub color: Option<Vec<u8>>,
    pub orientation: Vec<MetadataProperty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataProperty {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
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
    let xmp = find_xmp_item(encoded, iinf)?
        .map(|item_id| read_item(encoded, iloc, item_id))
        .transpose()?;
    let color = find_property(encoded, children_start, meta.end, *b"colr")?
        .map(|property| encoded[property.data..property.end].to_vec());
    let orientation = [*b"irot", *b"imir"]
        .into_iter()
        .filter_map(|kind| {
            find_property(encoded, children_start, meta.end, kind)
                .transpose()
                .map(|result| result.map(|property| (kind, property)))
        })
        .map(|result| {
            let (kind, property) = result?;
            Ok(MetadataProperty {
                kind,
                data: encoded[property.data..property.end].to_vec(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Metadata {
        exif,
        xmp,
        color,
        orientation,
    })
}

pub(crate) fn inject_metadata(encoded: &[u8], metadata: &Metadata) -> Result<Vec<u8>> {
    let mut output = encoded.to_vec();
    if let Some(color) = metadata.color.as_deref() {
        output = inject_property(&output, *b"colr", color, false)?;
    }
    for property in &metadata.orientation {
        if !matches!(&property.kind, b"irot" | b"imir") {
            return Err(invalid_avif("unsupported orientation property"));
        }
        output = inject_property(&output, property.kind, &property.data, true)?;
    }
    if let Some(xmp) = metadata.xmp.as_deref() {
        output = inject_xmp_item(&output, xmp)?;
    }
    Ok(output)
}

fn inject_property(encoded: &[u8], kind: [u8; 4], data: &[u8], essential: bool) -> Result<Vec<u8>> {
    let meta = find_box(encoded, 0, encoded.len(), *b"meta")?
        .ok_or_else(|| invalid_avif("missing meta box"))?;
    let children_start = meta
        .data
        .checked_add(4)
        .filter(|start| *start <= meta.end)
        .ok_or_else(|| invalid_avif("truncated meta full-box header"))?;
    let iprp = find_box(encoded, children_start, meta.end, *b"iprp")?
        .ok_or_else(|| invalid_avif("missing iprp box"))?;
    let ipco = find_box(encoded, iprp.data, iprp.end, *b"ipco")?
        .ok_or_else(|| invalid_avif("missing ipco box"))?;
    let current = find_box(encoded, ipco.data, ipco.end, kind)?;
    let property_box = make_box(kind, data)?;
    let new_ipco = if let Some(current) = current {
        rebuild_box(encoded, ipco, &[(current, property_box)])?
    } else {
        append_box(encoded, ipco, &property_box)?
    };

    let mut replacements = vec![(ipco, new_ipco)];
    if current.is_none() {
        let pitm = find_box(encoded, children_start, meta.end, *b"pitm")?
            .ok_or_else(|| invalid_avif("missing pitm box"))?;
        let primary_item = primary_item_id(encoded, pitm)?;
        let property_index = boxes(encoded, ipco.data, ipco.end)?.len() + 1;
        let ipma = find_box(encoded, iprp.data, iprp.end, *b"ipma")?
            .ok_or_else(|| invalid_avif("missing ipma box"))?;
        replacements.push((
            ipma,
            associate_property(encoded, ipma, primary_item, property_index, essential)?,
        ));
    }
    replacements.sort_by_key(|(range, _)| range.start);
    let new_iprp = rebuild_box(encoded, iprp, &replacements)?;
    let growth = new_iprp.len() - (iprp.end - iprp.start);
    let iloc = find_box(encoded, children_start, meta.end, *b"iloc")?
        .ok_or_else(|| invalid_avif("missing iloc box"))?;
    let new_iloc = shift_item_offsets(encoded, iloc, growth as u64)?;
    let mut meta_replacements = vec![(iloc, new_iloc), (iprp, new_iprp)];
    meta_replacements.sort_by_key(|(range, _)| range.start);
    let new_meta = rebuild_box(encoded, meta, &meta_replacements)?;
    let mut output = Vec::with_capacity(encoded.len() - (meta.end - meta.start) + new_meta.len());
    output.extend_from_slice(&encoded[..meta.start]);
    output.extend_from_slice(&new_meta);
    output.extend_from_slice(&encoded[meta.end..]);
    Ok(output)
}

fn inject_xmp_item(encoded: &[u8], xmp: &[u8]) -> Result<Vec<u8>> {
    if xmp.is_empty() {
        return Err(invalid_avif("XMP item contains no data"));
    }
    let meta = find_box(encoded, 0, encoded.len(), *b"meta")?
        .ok_or_else(|| invalid_avif("missing meta box"))?;
    let children_start = meta
        .data
        .checked_add(4)
        .filter(|start| *start <= meta.end)
        .ok_or_else(|| invalid_avif("truncated meta full-box header"))?;
    let iinf = find_box(encoded, children_start, meta.end, *b"iinf")?
        .ok_or_else(|| invalid_avif("missing iinf box"))?;
    if find_xmp_item(encoded, iinf)?.is_some() {
        return Err(invalid_avif("encoder output unexpectedly contains XMP"));
    }
    let iloc = find_box(encoded, children_start, meta.end, *b"iloc")?
        .ok_or_else(|| invalid_avif("missing iloc box"))?;
    let pitm = find_box(encoded, children_start, meta.end, *b"pitm")?
        .ok_or_else(|| invalid_avif("missing pitm box"))?;
    let primary_item = primary_item_id(encoded, pitm)?;
    let item_id = next_item_id(encoded, iinf)?;
    let mdat = find_box(encoded, meta.end, encoded.len(), *b"mdat")?
        .ok_or_else(|| invalid_avif("missing mdat box"))?;
    if meta.end > mdat.start {
        return Err(invalid_avif("meta box must precede mdat for XMP insertion"));
    }

    let infe = make_mime_infe(item_id)?;
    let reference = make_item_reference(*b"cdsc", item_id, primary_item)?;
    let iref = find_box(encoded, children_start, meta.end, *b"iref")?;
    let iref_growth = if iref.is_some() {
        reference.len()
    } else {
        reference.len() + 12
    };
    let iloc_growth = 14_usize;
    let meta_growth = infe
        .len()
        .checked_add(iloc_growth)
        .and_then(|growth| growth.checked_add(iref_growth))
        .ok_or_else(|| invalid_avif("XMP metadata size overflows"))?;
    let xmp_offset = u64::try_from(mdat.end)
        .ok()
        .and_then(|offset| offset.checked_add(meta_growth as u64))
        .ok_or_else(|| invalid_avif("XMP item offset overflows"))?;

    let new_iinf = append_iinf_item(encoded, iinf, &infe)?;
    let shifted_iloc = shift_item_offsets(encoded, iloc, meta_growth as u64)?;
    let new_iloc = append_iloc_item(&shifted_iloc, item_id, xmp_offset, xmp.len())?;
    let mut replacements = vec![(iinf, new_iinf), (iloc, new_iloc)];
    let new_iref = if let Some(iref) = iref {
        let replacement = append_box(encoded, iref, &reference)?;
        replacements.push((iref, replacement));
        None
    } else {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&reference);
        Some(make_box(*b"iref", &payload)?)
    };
    replacements.sort_by_key(|(range, _)| range.start);
    let mut new_meta = rebuild_box(encoded, meta, &replacements)?;
    if let Some(new_iref) = new_iref {
        new_meta.extend_from_slice(&new_iref);
        let size =
            u32::try_from(new_meta.len()).map_err(|_| invalid_avif("meta box is too large"))?;
        new_meta[..4].copy_from_slice(&size.to_be_bytes());
    }

    let old_mdat_size = read_u32(encoded, mdat.start)?;
    if old_mdat_size <= 1 {
        return Err(invalid_avif(
            "extended or unbounded mdat boxes are unsupported",
        ));
    }
    let new_mdat_size = usize::try_from(old_mdat_size)
        .ok()
        .and_then(|size| size.checked_add(xmp.len()))
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| invalid_avif("mdat size overflows"))?;
    let mut output = Vec::with_capacity(encoded.len() + meta_growth + xmp.len());
    output.extend_from_slice(&encoded[..meta.start]);
    output.extend_from_slice(&new_meta);
    output.extend_from_slice(&encoded[meta.end..mdat.start]);
    output.extend_from_slice(&new_mdat_size.to_be_bytes());
    output.extend_from_slice(&encoded[mdat.start + 4..mdat.end]);
    output.extend_from_slice(xmp);
    output.extend_from_slice(&encoded[mdat.end..]);
    Ok(output)
}

fn next_item_id(encoded: &[u8], iinf: BoxRange) -> Result<u32> {
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
    let mut maximum = 0_u32;
    for entry in entries {
        let item_id = match read_u8(encoded, entry.data)? {
            2 => u32::from(read_u16(encoded, entry.data + 4)?),
            3 => read_u32(encoded, entry.data + 4)?,
            _ => return Err(invalid_avif("unsupported infe version")),
        };
        maximum = maximum.max(item_id);
    }
    maximum
        .checked_add(1)
        .filter(|item_id| *item_id <= u32::from(u16::MAX))
        .ok_or_else(|| invalid_avif("no AVIF item identifier is available"))
}

fn make_mime_infe(item_id: u32) -> Result<Vec<u8>> {
    let item_id =
        u16::try_from(item_id).map_err(|_| invalid_avif("XMP item identifier is too large"))?;
    let mut payload = vec![2, 0, 0, 0];
    payload.extend_from_slice(&item_id.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    payload.extend_from_slice(b"mime");
    payload.push(0);
    payload.extend_from_slice(b"application/rdf+xml\0");
    make_box(*b"infe", &payload)
}

fn make_item_reference(kind: [u8; 4], from: u32, to: u32) -> Result<Vec<u8>> {
    let from =
        u16::try_from(from).map_err(|_| invalid_avif("reference item identifier is too large"))?;
    let to =
        u16::try_from(to).map_err(|_| invalid_avif("reference target identifier is too large"))?;
    let mut payload = from.to_be_bytes().to_vec();
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&to.to_be_bytes());
    make_box(kind, &payload)
}

fn append_iinf_item(encoded: &[u8], iinf: BoxRange, item: &[u8]) -> Result<Vec<u8>> {
    if read_u8(encoded, iinf.data)? != 0 {
        return Err(invalid_avif("XMP insertion requires iinf version 0"));
    }
    let count = read_u16(encoded, iinf.data + 4)?
        .checked_add(1)
        .ok_or_else(|| invalid_avif("too many iinf entries"))?;
    let mut output = encoded[iinf.start..iinf.end].to_vec();
    output[iinf.data + 4 - iinf.start..iinf.data + 6 - iinf.start]
        .copy_from_slice(&count.to_be_bytes());
    output.extend_from_slice(item);
    let size = u32::try_from(output.len()).map_err(|_| invalid_avif("iinf box is too large"))?;
    output[..4].copy_from_slice(&size.to_be_bytes());
    Ok(output)
}

fn append_iloc_item(
    shifted_iloc: &[u8],
    item_id: u32,
    extent_offset: u64,
    extent_length: usize,
) -> Result<Vec<u8>> {
    if shifted_iloc.len() < 16
        || shifted_iloc[8] != 0
        || shifted_iloc[12] != 0x44
        || shifted_iloc[13] != 0
    {
        return Err(invalid_avif(
            "XMP insertion requires the native iloc layout",
        ));
    }
    let item_id =
        u16::try_from(item_id).map_err(|_| invalid_avif("XMP item identifier is too large"))?;
    let extent_offset =
        u32::try_from(extent_offset).map_err(|_| invalid_avif("XMP extent offset is too large"))?;
    let extent_length =
        u32::try_from(extent_length).map_err(|_| invalid_avif("XMP item is too large"))?;
    let count = u16::from_be_bytes(shifted_iloc[14..16].try_into().unwrap())
        .checked_add(1)
        .ok_or_else(|| invalid_avif("too many iloc entries"))?;
    let mut output = shifted_iloc.to_vec();
    output[14..16].copy_from_slice(&count.to_be_bytes());
    output.extend_from_slice(&item_id.to_be_bytes());
    output.extend_from_slice(&0_u16.to_be_bytes());
    output.extend_from_slice(&1_u16.to_be_bytes());
    output.extend_from_slice(&extent_offset.to_be_bytes());
    output.extend_from_slice(&extent_length.to_be_bytes());
    let size = u32::try_from(output.len()).map_err(|_| invalid_avif("iloc box is too large"))?;
    output[..4].copy_from_slice(&size.to_be_bytes());
    Ok(output)
}

#[derive(Clone, Copy)]
struct BoxRange {
    start: usize,
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
            start: offset,
            data: offset + header,
            end: box_end,
        });
        offset = box_end;
    }
    Ok(result)
}

fn find_property(
    encoded: &[u8],
    meta_start: usize,
    meta_end: usize,
    kind: [u8; 4],
) -> Result<Option<BoxRange>> {
    let Some(iprp) = find_box(encoded, meta_start, meta_end, *b"iprp")? else {
        return Ok(None);
    };
    let Some(ipco) = find_box(encoded, iprp.data, iprp.end, *b"ipco")? else {
        return Ok(None);
    };
    find_box(encoded, ipco.data, ipco.end, kind)
}

fn make_box(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>> {
    let size = u32::try_from(payload.len() + 8).map_err(|_| invalid_avif("box is too large"))?;
    let mut output = Vec::with_capacity(payload.len() + 8);
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(payload);
    Ok(output)
}

fn rebuild_box(
    encoded: &[u8],
    parent: BoxRange,
    replacements: &[(BoxRange, Vec<u8>)],
) -> Result<Vec<u8>> {
    let mut output = encoded[parent.start..parent.data].to_vec();
    let mut cursor = parent.data;
    for (child, replacement) in replacements {
        if child.start < cursor || child.end > parent.end {
            return Err(invalid_avif("replacement box is outside its parent"));
        }
        output.extend_from_slice(&encoded[cursor..child.start]);
        output.extend_from_slice(replacement);
        cursor = child.end;
    }
    output.extend_from_slice(&encoded[cursor..parent.end]);
    let size = u32::try_from(output.len()).map_err(|_| invalid_avif("box is too large"))?;
    output[..4].copy_from_slice(&size.to_be_bytes());
    Ok(output)
}

fn append_box(encoded: &[u8], parent: BoxRange, child: &[u8]) -> Result<Vec<u8>> {
    let mut output = encoded[parent.start..parent.end].to_vec();
    output.extend_from_slice(child);
    let size = u32::try_from(output.len()).map_err(|_| invalid_avif("box is too large"))?;
    output[..4].copy_from_slice(&size.to_be_bytes());
    Ok(output)
}

fn primary_item_id(encoded: &[u8], pitm: BoxRange) -> Result<u32> {
    match read_u8(encoded, pitm.data)? {
        0 => Ok(u32::from(read_u16(encoded, pitm.data + 4)?)),
        1 => read_u32(encoded, pitm.data + 4),
        _ => Err(invalid_avif("unsupported pitm version")),
    }
}

fn associate_property(
    encoded: &[u8],
    ipma: BoxRange,
    expected_item: u32,
    property_index: usize,
    essential: bool,
) -> Result<Vec<u8>> {
    let version = read_u8(encoded, ipma.data)?;
    let flags = read_u32(encoded, ipma.data)? & 0x00ff_ffff;
    let wide_association = flags & 1 != 0;
    let association_size = if wide_association { 2 } else { 1 };
    let property_limit = if wide_association { 0x7fff } else { 0x7f };
    if property_index == 0 || property_index > property_limit {
        return Err(invalid_avif("color property index exceeds ipma limits"));
    }
    let entry_count = read_u32(encoded, ipma.data + 4)?;
    let mut cursor = ipma.data + 8;
    let mut insertion = None;
    let mut count_offset = 0;
    for _ in 0..entry_count {
        let item_id = match version {
            0 => {
                let value = u32::from(read_u16(encoded, cursor)?);
                cursor += 2;
                value
            }
            1 => {
                let value = read_u32(encoded, cursor)?;
                cursor += 4;
                value
            }
            _ => return Err(invalid_avif("unsupported ipma version")),
        };
        let association_count = usize::from(read_u8(encoded, cursor)?);
        if item_id == expected_item {
            count_offset = cursor;
            insertion = Some(cursor + 1 + association_count * association_size);
        }
        cursor = cursor
            .checked_add(1 + association_count * association_size)
            .filter(|cursor| *cursor <= ipma.end)
            .ok_or_else(|| invalid_avif("ipma associations exceed box bounds"))?;
    }
    let insertion = insertion.ok_or_else(|| invalid_avif("primary item is missing from ipma"))?;
    let count = read_u8(encoded, count_offset)?
        .checked_add(1)
        .ok_or_else(|| invalid_avif("too many ipma associations"))?;
    let mut payload = encoded[ipma.data..ipma.end].to_vec();
    payload[count_offset - ipma.data] = count;
    let association = if wide_association {
        let value = u16::try_from(property_index).unwrap() | if essential { 0x8000 } else { 0 };
        value.to_be_bytes().to_vec()
    } else {
        vec![(property_index as u8) | if essential { 0x80 } else { 0 }]
    };
    payload.splice(insertion - ipma.data..insertion - ipma.data, association);
    make_box(*b"ipma", &payload)
}

fn shift_item_offsets(encoded: &[u8], iloc: BoxRange, delta: u64) -> Result<Vec<u8>> {
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
    let mut payload = encoded[iloc.data..iloc.end].to_vec();
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
        cursor += if version < 2 { 2 } else { 4 };
        let construction_method = if version == 0 {
            0
        } else {
            let value = read_u16(encoded, cursor)? & 0xf;
            cursor += 2;
            value
        };
        let data_reference = read_u16(encoded, cursor)?;
        cursor += 2;
        let base_position = cursor;
        let base_offset = read_sized(encoded, &mut cursor, base_offset_size)?;
        let extent_count = read_u16(encoded, cursor)?;
        cursor += 2;
        for _ in 0..extent_count {
            let _index = read_sized(encoded, &mut cursor, index_size)?;
            let offset_position = cursor;
            let extent_offset = read_sized(encoded, &mut cursor, offset_size)?;
            let _length = read_sized(encoded, &mut cursor, length_size)?;
            if construction_method == 0 && data_reference == 0 && base_offset_size == 0 {
                write_sized(
                    &mut payload,
                    offset_position - iloc.data,
                    offset_size,
                    extent_offset
                        .checked_add(delta)
                        .ok_or_else(|| invalid_avif("iloc extent offset overflows"))?,
                )?;
            }
        }
        if construction_method == 0 && data_reference == 0 && base_offset_size > 0 {
            write_sized(
                &mut payload,
                base_position - iloc.data,
                base_offset_size,
                base_offset
                    .checked_add(delta)
                    .ok_or_else(|| invalid_avif("iloc base offset overflows"))?,
            )?;
        }
    }
    make_box(*b"iloc", &payload)
}

fn write_sized(output: &mut [u8], offset: usize, size: usize, value: u64) -> Result<()> {
    if !matches!(size, 0 | 4 | 8) || (size == 0 && value != 0) {
        return Err(invalid_avif("unsupported iloc integer size"));
    }
    if size == 4 && value > u64::from(u32::MAX) {
        return Err(invalid_avif("iloc offset exceeds its encoded size"));
    }
    let bytes = value.to_be_bytes();
    output[offset..offset + size].copy_from_slice(&bytes[8 - size..]);
    Ok(())
}

fn find_box(encoded: &[u8], start: usize, end: usize, kind: [u8; 4]) -> Result<Option<BoxRange>> {
    Ok(boxes(encoded, start, end)?
        .into_iter()
        .find(|range| encoded[range.data - 4..range.data] == kind))
}

fn find_exif_item(encoded: &[u8], iinf: BoxRange) -> Result<Option<u32>> {
    find_metadata_item(encoded, iinf, *b"Exif", None)
}

fn find_xmp_item(encoded: &[u8], iinf: BoxRange) -> Result<Option<u32>> {
    find_metadata_item(encoded, iinf, *b"mime", Some(b"application/rdf+xml"))
}

fn find_metadata_item(
    encoded: &[u8],
    iinf: BoxRange,
    expected_type: [u8; 4],
    expected_content_type: Option<&[u8]>,
) -> Result<Option<u32>> {
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
    let mut found_id = None;
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
        if read_fourcc(encoded, type_offset)? != expected_type {
            continue;
        }
        if let Some(expected_content_type) = expected_content_type {
            let name_end = encoded[type_offset + 4..entry.end]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| type_offset + 4 + offset)
                .ok_or_else(|| invalid_avif("metadata item name is not terminated"))?;
            let content_start = name_end + 1;
            let content_end = encoded[content_start..entry.end]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| content_start + offset)
                .unwrap_or(entry.end);
            if &encoded[content_start..content_end] != expected_content_type {
                continue;
            }
        }
        if found_id.replace(item_id).is_some() {
            return Err(invalid_avif(
                "multiple matching metadata items are unsupported",
            ));
        }
    }
    Ok(found_id)
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

#[cfg(test)]
mod tests {
    use image::{codecs::avif::AvifEncoder, ExtendedColorType, ImageEncoder};

    use super::*;

    fn fixture() -> Vec<u8> {
        let mut encoded = Vec::new();
        AvifEncoder::new_with_speed_quality(&mut encoded, 10, 80)
            .write_image(&[10, 20, 30, 255], 1, 1, ExtendedColorType::Rgba8)
            .unwrap();
        encoded
    }

    #[test]
    fn inserts_and_associates_color_property() {
        let input = fixture();
        let color = [b"nclx".as_slice(), &[0, 1, 0, 13, 0, 6, 0x80]].concat();
        let metadata = Metadata {
            exif: None,
            xmp: None,
            color: Some(color.clone()),
            orientation: Vec::new(),
        };

        let output = inject_metadata(&input, &metadata).unwrap();

        assert_eq!(extract_metadata(&output).unwrap().color, Some(color));
        image::load_from_memory(&output).unwrap();
    }

    #[test]
    fn inserts_essential_orientation_properties() {
        let input = fixture();
        let orientation = vec![
            MetadataProperty {
                kind: *b"irot",
                data: vec![1],
            },
            MetadataProperty {
                kind: *b"imir",
                data: vec![1],
            },
        ];
        let metadata = Metadata {
            exif: None,
            xmp: None,
            color: None,
            orientation: orientation.clone(),
        };

        let output = inject_metadata(&input, &metadata).unwrap();

        assert_eq!(extract_metadata(&output).unwrap().orientation, orientation);
        image::load_from_memory(&output).unwrap();
    }

    #[test]
    fn inserts_xmp_metadata_item() {
        let input = fixture();
        let xmp = b"<x:xmpmeta>fixture</x:xmpmeta>".to_vec();
        let metadata = Metadata {
            exif: None,
            xmp: Some(xmp.clone()),
            color: None,
            orientation: Vec::new(),
        };

        let output = inject_metadata(&input, &metadata).unwrap();

        assert_eq!(extract_metadata(&output).unwrap().xmp, Some(xmp));
        image::load_from_memory(&output).unwrap();
    }
}
