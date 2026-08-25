use std::array;

use crate::{DatabenderError, Result};

#[derive(Clone, Debug)]
pub(crate) struct AcUsage {
    pub(crate) tables: [AcTableUsage; 4],
}

#[derive(Clone, Debug)]
pub(crate) struct AcTableUsage {
    pub(crate) counts: [u64; 256],
    pub(crate) luma: bool,
    pub(crate) chroma: bool,
}

impl Default for AcTableUsage {
    fn default() -> Self {
        Self {
            counts: [0; 256],
            luma: false,
            chroma: false,
        }
    }
}

impl Default for AcUsage {
    fn default() -> Self {
        Self {
            tables: array::from_fn(|_| AcTableUsage::default()),
        }
    }
}

pub(crate) fn analyze_ac_usage(input: &[u8]) -> Result<Option<AcUsage>> {
    if !input.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("missing SOI marker"));
    }

    let mut offset = 2;
    let mut frame = None;
    let mut dc_tables: [Option<HuffmanTable>; 4] = array::from_fn(|_| None);
    let mut ac_tables: [Option<HuffmanTable>; 4] = array::from_fn(|_| None);
    let mut restart_interval = 0_usize;
    let mut usage = AcUsage::default();
    let mut saw_scan = false;

    while offset < input.len() {
        let (marker, marker_end) = read_marker(input, offset)?;
        offset = marker_end;
        if marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let (segment, segment_end) = read_segment(input, offset)?;
        match marker {
            0xc0 => frame = Some(parse_frame(segment)?),
            0xc2 => return Ok(None),
            0xc4 => parse_huffman_tables(segment, &mut dc_tables, &mut ac_tables)?,
            0xdd => {
                if segment.len() != 2 {
                    return Err(invalid_jpeg("DRI segment must contain two bytes"));
                }
                restart_interval = usize::from(u16::from_be_bytes([segment[0], segment[1]]));
            }
            0xda => {
                let frame = frame
                    .as_ref()
                    .ok_or_else(|| invalid_jpeg("SOS appeared before baseline SOF"))?;
                let scan = parse_scan(segment, frame, &mut usage)?;
                let entropy_end = find_entropy_end(input, segment_end)?;
                decode_scan(
                    &input[segment_end..entropy_end],
                    frame,
                    &scan,
                    &dc_tables,
                    &ac_tables,
                    restart_interval,
                    &mut usage,
                )?;
                saw_scan = true;
                offset = entropy_end;
                continue;
            }
            _ => {}
        }
        offset = segment_end;
    }

    if !saw_scan {
        return Err(invalid_jpeg("missing baseline scan"));
    }
    Ok(Some(usage))
}

#[derive(Clone, Debug)]
struct Frame {
    width: usize,
    height: usize,
    max_horizontal: usize,
    max_vertical: usize,
    components: Vec<FrameComponent>,
}

#[derive(Clone, Debug)]
struct FrameComponent {
    id: u8,
    horizontal: usize,
    vertical: usize,
    luma: bool,
}

fn parse_frame(segment: &[u8]) -> Result<Frame> {
    if segment.len() < 6 || segment[0] != 8 {
        return Err(invalid_jpeg("unsupported baseline frame header"));
    }
    let height = usize::from(u16::from_be_bytes([segment[1], segment[2]]));
    let width = usize::from(u16::from_be_bytes([segment[3], segment[4]]));
    let component_count = usize::from(segment[5]);
    if width == 0 || height == 0 || component_count == 0 || segment.len() != 6 + component_count * 3
    {
        return Err(invalid_jpeg("invalid baseline frame geometry"));
    }

    let mut components = Vec::with_capacity(component_count);
    for index in 0..component_count {
        let start = 6 + index * 3;
        let sampling = segment[start + 1];
        let horizontal = usize::from(sampling >> 4);
        let vertical = usize::from(sampling & 0x0f);
        if horizontal == 0 || vertical == 0 {
            return Err(invalid_jpeg("component sampling factors must be nonzero"));
        }
        components.push(FrameComponent {
            id: segment[start],
            horizontal,
            vertical,
            luma: index == 0,
        });
    }
    let max_horizontal = components
        .iter()
        .map(|component| component.horizontal)
        .max()
        .expect("frame has at least one component");
    let max_vertical = components
        .iter()
        .map(|component| component.vertical)
        .max()
        .expect("frame has at least one component");
    Ok(Frame {
        width,
        height,
        max_horizontal,
        max_vertical,
        components,
    })
}

#[derive(Clone, Debug)]
struct ScanComponent {
    frame_index: usize,
    dc_table: usize,
    ac_table: usize,
}

fn parse_scan(segment: &[u8], frame: &Frame, usage: &mut AcUsage) -> Result<Vec<ScanComponent>> {
    let component_count = segment.first().copied().map(usize::from).unwrap_or(0);
    let parameters_start = 1 + component_count * 2;
    if component_count == 0 || segment.len() != parameters_start + 3 {
        return Err(invalid_jpeg("invalid SOS component list"));
    }
    if segment[parameters_start..] != [0, 63, 0] {
        return Err(invalid_jpeg("unsupported non-sequential scan parameters"));
    }

    let mut components = Vec::with_capacity(component_count);
    for index in 0..component_count {
        let start = 1 + index * 2;
        let frame_index = frame
            .components
            .iter()
            .position(|component| component.id == segment[start])
            .ok_or_else(|| invalid_jpeg("SOS references an unknown component"))?;
        let selectors = segment[start + 1];
        let dc_table = usize::from(selectors >> 4);
        let ac_table = usize::from(selectors & 0x0f);
        if dc_table >= 4 || ac_table >= 4 {
            return Err(invalid_jpeg("Huffman table selector exceeds JPEG limits"));
        }
        if frame.components[frame_index].luma {
            usage.tables[ac_table].luma = true;
        } else {
            usage.tables[ac_table].chroma = true;
        }
        components.push(ScanComponent {
            frame_index,
            dc_table,
            ac_table,
        });
    }
    Ok(components)
}

fn decode_scan(
    entropy: &[u8],
    frame: &Frame,
    scan: &[ScanComponent],
    dc_tables: &[Option<HuffmanTable>; 4],
    ac_tables: &[Option<HuffmanTable>; 4],
    restart_interval: usize,
    usage: &mut AcUsage,
) -> Result<()> {
    let interleaved = scan.len() > 1;
    let mcu_count = if interleaved {
        divide_round_up(frame.width, 8 * frame.max_horizontal)
            * divide_round_up(frame.height, 8 * frame.max_vertical)
    } else {
        let component = &frame.components[scan[0].frame_index];
        let component_width =
            divide_round_up(frame.width * component.horizontal, frame.max_horizontal);
        let component_height =
            divide_round_up(frame.height * component.vertical, frame.max_vertical);
        divide_round_up(component_width, 8) * divide_round_up(component_height, 8)
    };
    let mut reader = EntropyReader::new(entropy);

    for mcu_index in 0..mcu_count {
        if restart_interval > 0 && mcu_index > 0 && mcu_index % restart_interval == 0 {
            reader.align_to_byte();
        }
        for scan_component in scan {
            let component = &frame.components[scan_component.frame_index];
            let block_count = if interleaved {
                component.horizontal * component.vertical
            } else {
                1
            };
            let dc_table = dc_tables[scan_component.dc_table]
                .as_ref()
                .ok_or_else(|| invalid_jpeg("scan references a missing DC Huffman table"))?;
            let ac_table = ac_tables[scan_component.ac_table]
                .as_ref()
                .ok_or_else(|| invalid_jpeg("scan references a missing AC Huffman table"))?;
            for _ in 0..block_count {
                decode_block(
                    &mut reader,
                    dc_table,
                    ac_table,
                    &mut usage.tables[scan_component.ac_table].counts,
                )?;
            }
        }
    }
    Ok(())
}

fn decode_block(
    reader: &mut EntropyReader<'_>,
    dc_table: &HuffmanTable,
    ac_table: &HuffmanTable,
    counts: &mut [u64; 256],
) -> Result<()> {
    let dc_size = dc_table.decode(reader)?;
    if dc_size > 11 {
        return Err(invalid_jpeg("invalid baseline DC coefficient size"));
    }
    reader.skip_bits(dc_size)?;

    let mut coefficient = 1_usize;
    while coefficient < 64 {
        let symbol = ac_table.decode(reader)?;
        counts[usize::from(symbol)] += 1;
        if symbol == 0 {
            break;
        }
        if symbol == 0xf0 {
            coefficient += 16;
            continue;
        }
        let run = usize::from(symbol >> 4);
        let size = symbol & 0x0f;
        if size == 0 || size > 10 {
            return Err(invalid_jpeg("invalid baseline AC symbol"));
        }
        coefficient += run;
        if coefficient >= 64 {
            return Err(invalid_jpeg("AC run exceeds coefficient block"));
        }
        reader.skip_bits(size)?;
        coefficient += 1;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct HuffmanTable {
    entries: Vec<HuffmanCode>,
}

#[derive(Clone, Copy, Debug)]
struct HuffmanCode {
    bits: u16,
    length: u8,
    symbol: u8,
}

impl HuffmanTable {
    fn decode(&self, reader: &mut EntropyReader<'_>) -> Result<u8> {
        let mut bits = 0_u16;
        for length in 1..=16 {
            bits = (bits << 1) | u16::from(reader.read_bit()?);
            if let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.length == length && entry.bits == bits)
            {
                return Ok(entry.symbol);
            }
        }
        Err(invalid_jpeg(
            "entropy data contains an unknown Huffman code",
        ))
    }
}

fn parse_huffman_tables(
    segment: &[u8],
    dc_tables: &mut [Option<HuffmanTable>; 4],
    ac_tables: &mut [Option<HuffmanTable>; 4],
) -> Result<()> {
    let mut offset = 0;
    while offset < segment.len() {
        if segment.len() - offset < 17 {
            return Err(invalid_jpeg("truncated Huffman table"));
        }
        let class = segment[offset] >> 4;
        let identifier = usize::from(segment[offset] & 0x0f);
        if class > 1 || identifier >= 4 {
            return Err(invalid_jpeg("invalid Huffman table selector"));
        }
        let counts = &segment[offset + 1..offset + 17];
        let symbol_count = counts
            .iter()
            .map(|count| usize::from(*count))
            .sum::<usize>();
        let symbols_start = offset + 17;
        let symbols_end = symbols_start
            .checked_add(symbol_count)
            .filter(|end| *end <= segment.len())
            .ok_or_else(|| invalid_jpeg("truncated Huffman symbols"))?;
        let mut entries = Vec::with_capacity(symbol_count);
        let mut code = 0_u16;
        let mut symbol_index = symbols_start;
        for (index, count) in counts.iter().enumerate() {
            let length = (index + 1) as u8;
            for _ in 0..*count {
                entries.push(HuffmanCode {
                    bits: code,
                    length,
                    symbol: segment[symbol_index],
                });
                code = code
                    .checked_add(1)
                    .ok_or_else(|| invalid_jpeg("Huffman code overflow"))?;
                symbol_index += 1;
            }
            if index < 15 {
                code = code
                    .checked_shl(1)
                    .ok_or_else(|| invalid_jpeg("Huffman code overflow"))?;
            }
        }
        let table = HuffmanTable { entries };
        if class == 0 {
            dc_tables[identifier] = Some(table);
        } else {
            ac_tables[identifier] = Some(table);
        }
        offset = symbols_end;
    }
    Ok(())
}

struct EntropyReader<'a> {
    encoded: &'a [u8],
    offset: usize,
    current: u8,
    remaining: u8,
}

impl<'a> EntropyReader<'a> {
    fn new(encoded: &'a [u8]) -> Self {
        Self {
            encoded,
            offset: 0,
            current: 0,
            remaining: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u8> {
        if self.remaining == 0 {
            self.current = self.read_byte()?;
            self.remaining = 8;
        }
        self.remaining -= 1;
        Ok((self.current >> self.remaining) & 1)
    }

    fn skip_bits(&mut self, count: u8) -> Result<()> {
        for _ in 0..count {
            self.read_bit()?;
        }
        Ok(())
    }

    fn align_to_byte(&mut self) {
        self.remaining = 0;
    }

    fn read_byte(&mut self) -> Result<u8> {
        loop {
            let byte = *self
                .encoded
                .get(self.offset)
                .ok_or_else(|| invalid_jpeg("entropy data ended inside a scan"))?;
            self.offset += 1;
            if byte != 0xff {
                return Ok(byte);
            }
            while self.encoded.get(self.offset) == Some(&0xff) {
                self.offset += 1;
            }
            let marker = *self
                .encoded
                .get(self.offset)
                .ok_or_else(|| invalid_jpeg("truncated entropy marker"))?;
            self.offset += 1;
            match marker {
                0x00 => return Ok(0xff),
                0xd0..=0xd7 => continue,
                _ => return Err(invalid_jpeg("unexpected marker inside entropy data")),
            }
        }
    }
}

fn read_marker(input: &[u8], mut offset: usize) -> Result<(u8, usize)> {
    if input.get(offset) != Some(&0xff) {
        return Err(invalid_jpeg("expected marker prefix"));
    }
    while input.get(offset) == Some(&0xff) {
        offset += 1;
    }
    let marker = *input
        .get(offset)
        .ok_or_else(|| invalid_jpeg("truncated marker"))?;
    if marker == 0x00 {
        return Err(invalid_jpeg("unexpected stuffed byte outside entropy data"));
    }
    Ok((marker, offset + 1))
}

fn read_segment(input: &[u8], offset: usize) -> Result<(&[u8], usize)> {
    if input.len().saturating_sub(offset) < 2 {
        return Err(invalid_jpeg("truncated segment length"));
    }
    let length = usize::from(u16::from_be_bytes([input[offset], input[offset + 1]]));
    if length < 2 {
        return Err(invalid_jpeg("segment length is smaller than its header"));
    }
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= input.len())
        .ok_or_else(|| invalid_jpeg("segment length exceeds file size"))?;
    Ok((&input[offset + 2..end], end))
}

fn find_entropy_end(input: &[u8], mut offset: usize) -> Result<usize> {
    while offset + 1 < input.len() {
        if input[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker_start = offset;
        while input.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *input
            .get(offset)
            .ok_or_else(|| invalid_jpeg("truncated entropy marker"))?;
        match marker {
            0x00 | 0xd0..=0xd7 => offset += 1,
            _ => return Ok(marker_start),
        }
    }
    Err(invalid_jpeg("scan is missing a following marker"))
}

fn divide_round_up(value: usize, divisor: usize) -> usize {
    value.div_ceil(divisor)
}

fn invalid_jpeg(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid JPEG entropy data: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use image::{codecs::jpeg::JpegEncoder, ImageBuffer, Rgb};

    use super::*;

    #[test]
    fn counts_baseline_ac_symbols_and_component_roles() {
        let image = ImageBuffer::from_fn(32, 24, |x, y| {
            Rgb([(x * 7) as u8, (y * 11) as u8, ((x + y) * 5) as u8])
        });
        let mut encoded = Vec::new();
        JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&image)
            .unwrap();

        let usage = analyze_ac_usage(&encoded).unwrap().unwrap();

        assert!(usage.tables[0].luma);
        assert!(usage.tables[1].chroma);
        assert!(usage.tables[0].counts.iter().sum::<u64>() > 0);
        assert!(usage.tables[1].counts.iter().sum::<u64>() > 0);
    }

    #[test]
    fn declines_progressive_entropy_analysis() {
        let progressive = [
            0xff, 0xd8, 0xff, 0xc2, 0x00, 0x08, 8, 0, 1, 0, 1, 0, 0xff, 0xd9,
        ];

        assert!(analyze_ac_usage(&progressive).unwrap().is_none());
    }
}
