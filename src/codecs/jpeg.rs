use crate::{
    codecs::jpeg_entropy::{analyze_ac_usage, reconstruct_coefficients, AcUsage, CoefficientImage},
    DatabenderError, HuffmanGlitchMode, HuffmanTarget, Result,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HuffmanGlitchOptions {
    pub(crate) swaps: usize,
    pub(crate) intensity: f64,
    pub(crate) target: HuffmanTarget,
    pub(crate) mode: HuffmanGlitchMode,
    pub(crate) preserve_size: bool,
    pub(crate) scan_start: usize,
    pub(crate) scan_count: usize,
    pub(crate) frequency_start: usize,
    pub(crate) frequency_end: usize,
}

pub(crate) fn mutate_huffman_tables(
    input: &[u8],
    options: HuffmanGlitchOptions,
    seed: u64,
) -> Result<Vec<u8>> {
    if !input.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("missing SOI marker"));
    }

    let usage = analyze_ac_usage(input).ok().flatten();
    let swaps = if options.intensity == 0.0 {
        0
    } else {
        ((options.swaps as f64) * options.intensity.powi(2))
            .round()
            .max(1.0) as usize
    };
    let mut output = input.to_vec();
    let mut offset = 2;
    let mut rng = SeededRng::new(seed);
    let mut tables_found = 0_usize;
    while offset < output.len() {
        if output[offset] != 0xff {
            return Err(invalid_jpeg("expected marker prefix before scan data"));
        }
        while offset < output.len() && output[offset] == 0xff {
            offset += 1;
        }
        if offset >= output.len() {
            return Err(invalid_jpeg("truncated marker"));
        }
        let marker = output[offset];
        offset += 1;
        if marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if output.len() - offset < 2 {
            return Err(invalid_jpeg("truncated segment length"));
        }
        let length = usize::from(u16::from_be_bytes([output[offset], output[offset + 1]]));
        let segment_end = offset
            .checked_add(length)
            .filter(|end| *end <= output.len())
            .ok_or_else(|| invalid_jpeg("segment length exceeds file size"))?;
        if marker == 0xc4 {
            tables_found += mutate_dht_segment(
                &mut output[offset + 2..segment_end],
                swaps,
                options,
                usage.as_ref(),
                &mut rng,
            )?;
        }
        offset = segment_end;
        if marker == 0xda {
            while offset + 1 < output.len() {
                if output[offset] != 0xff {
                    offset += 1;
                    continue;
                }
                match output[offset + 1] {
                    0x00 | 0xd0..=0xd7 => offset += 2,
                    _ => break,
                }
            }
        }
    }

    if tables_found == 0 {
        return Err(invalid_jpeg("missing AC Huffman table"));
    }
    Ok(output)
}

pub(crate) fn mutate_coefficients(
    input: &[u8],
    options: HuffmanGlitchOptions,
    seed: u64,
) -> Result<Vec<u8>> {
    let progressive = has_frame_marker(input, 0xc2)?;
    let (source, mut coefficients, scan_origins) = if progressive {
        let reconstructed = reconstruct_coefficients(input)?;
        let source = baseline_skeleton(input, &reconstructed)?;
        let scan_origins = reconstructed
            .components
            .iter()
            .map(|component| component.scans.clone())
            .collect::<Vec<_>>();
        let coefficients = dct_io::JpegCoefficients {
            components: reconstructed
                .components
                .into_iter()
                .map(|component| dct_io::ComponentCoefficients {
                    id: component.id,
                    blocks: component.blocks,
                })
                .collect(),
        };
        (source, coefficients, Some(scan_origins))
    } else {
        let coefficients = dct_io::read_coefficients(input)
            .map_err(|error| invalid_jpeg(format!("coefficient decode failed: {error}")))?;
        (input.to_vec(), coefficients, None)
    };

    let mutations = if options.intensity == 0.0 {
        0
    } else {
        ((options.swaps as f64) * options.intensity.powi(2))
            .round()
            .max(1.0) as usize
    };
    let mut rng = SeededRng::new(seed);
    let eligible_components = coefficients
        .components
        .iter()
        .enumerate()
        .filter(|(index, _)| match options.target {
            HuffmanTarget::All => true,
            HuffmanTarget::LumaAc => *index == 0,
            HuffmanTarget::ChromaAc => *index > 0,
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if eligible_components.is_empty() {
        return Err(invalid_jpeg("coefficient target selects no components"));
    }

    for _ in 0..mutations {
        let component_index = eligible_components[rng.index(eligible_components.len())];
        let component = &mut coefficients.components[component_index];
        if component.blocks.is_empty() {
            continue;
        }
        let block_index = rng.index(component.blocks.len());
        let block = &mut component.blocks[block_index];
        let scan_end = options.scan_start.saturating_add(options.scan_count);
        let populated = (options.frequency_start..=options.frequency_end)
            .filter(|index| {
                if block[*index] == 0 {
                    return false;
                }
                let scan = scan_origins.as_ref().map_or(0, |origins| {
                    usize::from(origins[component_index][block_index][*index])
                });
                options.scan_count == 0 || (scan >= options.scan_start && scan < scan_end)
            })
            .collect::<Vec<_>>();
        if populated.is_empty() {
            continue;
        }
        let source_index = populated[rng.index(populated.len())];
        match options.mode {
            HuffmanGlitchMode::SymbolRemap => block[source_index] = -block[source_index],
            HuffmanGlitchMode::RunRemap => {
                let distance = ((options.intensity * 15.0).ceil() as usize).max(1);
                let start = source_index
                    .saturating_sub(distance)
                    .max(options.frequency_start);
                let end = (source_index + distance).min(options.frequency_end);
                let empty = (start..=end)
                    .filter(|index| block[*index] == 0)
                    .collect::<Vec<_>>();
                if !empty.is_empty() {
                    let destination = empty[rng.index(empty.len())];
                    block[destination] = block[source_index];
                    block[source_index] = 0;
                }
            }
        }
    }

    dct_io::write_coefficients(&source, &coefficients)
        .map_err(|error| invalid_jpeg(format!("coefficient encode failed: {error}")))
}

fn has_frame_marker(input: &[u8], expected: u8) -> Result<bool> {
    if !input.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("missing SOI marker"));
    }
    let mut offset = 2;
    while offset + 4 <= input.len() {
        if input[offset] != 0xff {
            return Err(invalid_jpeg("expected marker prefix"));
        }
        let marker = input[offset + 1];
        if marker == expected {
            return Ok(true);
        }
        if marker == 0xda || marker == 0xd9 {
            return Ok(false);
        }
        let length = usize::from(u16::from_be_bytes([input[offset + 2], input[offset + 3]]));
        offset = offset
            .checked_add(length + 2)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| invalid_jpeg("segment length exceeds file size"))?;
    }
    Err(invalid_jpeg("missing frame marker"))
}

fn baseline_skeleton(input: &[u8], coefficients: &CoefficientImage) -> Result<Vec<u8>> {
    let estimated_entropy = coefficients.width.saturating_mul(coefficients.height) / 2;
    let mut output = Vec::with_capacity(input.len().max(estimated_entropy));
    output.extend([0xff, 0xd8]);
    let mut offset = 2;
    let mut restart_interval = 0_usize;
    let mut found_frame = false;
    while offset + 4 <= input.len() {
        if input[offset] != 0xff {
            return Err(invalid_jpeg(
                "expected marker prefix before progressive scan",
            ));
        }
        let marker = input[offset + 1];
        if marker == 0xda {
            break;
        }
        let length = usize::from(u16::from_be_bytes([input[offset + 2], input[offset + 3]]));
        let end = offset
            .checked_add(length + 2)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| invalid_jpeg("segment length exceeds file size"))?;
        if marker == 0xdd && length == 4 {
            restart_interval =
                usize::from(u16::from_be_bytes([input[offset + 4], input[offset + 5]]));
        }
        if marker != 0xc4 {
            output.extend_from_slice(&input[offset..end]);
            if marker == 0xc2 {
                let marker_index = output.len() - (end - offset) + 1;
                output[marker_index] = 0xc0;
                found_frame = true;
            }
        }
        offset = end;
    }
    if !found_frame {
        return Err(invalid_jpeg("missing progressive frame"));
    }

    let huffman_segments = standard_huffman_segments()?;
    for segment in &huffman_segments {
        output.extend_from_slice(segment);
    }
    let mut scan = Vec::with_capacity(1 + coefficients.components.len() * 2 + 3);
    scan.push(coefficients.components.len() as u8);
    for component in &coefficients.components {
        scan.push(component.id);
        scan.push(if component.luma { 0x00 } else { 0x11 });
    }
    scan.extend([0, 63, 0]);
    append_segment(&mut output, 0xda, &scan)?;
    append_zero_entropy(
        &mut output,
        coefficients,
        &huffman_segments,
        restart_interval,
    )?;
    output.extend([0xff, 0xd9]);
    Ok(output)
}

fn standard_huffman_segments() -> Result<Vec<Vec<u8>>> {
    let image = image::ImageBuffer::from_pixel(1, 1, image::Rgb([128_u8, 128, 128]));
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&image)
        .map_err(|error| invalid_jpeg(format!("failed to build standard tables: {error}")))?;
    let mut segments = Vec::new();
    let mut offset = 2;
    while offset + 4 <= encoded.len() {
        let marker = encoded[offset + 1];
        if marker == 0xda {
            break;
        }
        let length = usize::from(u16::from_be_bytes([
            encoded[offset + 2],
            encoded[offset + 3],
        ]));
        let end = offset + length + 2;
        if marker == 0xc4 {
            segments.push(encoded[offset..end].to_vec());
        }
        offset = end;
    }
    if segments.is_empty() {
        return Err(invalid_jpeg("standard encoder emitted no Huffman tables"));
    }
    Ok(segments)
}

#[derive(Clone, Copy)]
struct EncodeCode {
    bits: u16,
    length: u8,
}

fn zero_code(segments: &[Vec<u8>], selector: u8) -> Result<EncodeCode> {
    for segment in segments {
        let mut offset = 4;
        while offset < segment.len() {
            let table_selector = segment[offset];
            let counts = &segment[offset + 1..offset + 17];
            let symbol_count = counts
                .iter()
                .map(|count| usize::from(*count))
                .sum::<usize>();
            let symbols = &segment[offset + 17..offset + 17 + symbol_count];
            let mut code = 0_u16;
            let mut symbol_index = 0;
            for (length_index, count) in counts.iter().enumerate() {
                for _ in 0..*count {
                    if table_selector == selector && symbols[symbol_index] == 0 {
                        return Ok(EncodeCode {
                            bits: code,
                            length: (length_index + 1) as u8,
                        });
                    }
                    code += 1;
                    symbol_index += 1;
                }
                code <<= 1;
            }
            offset += 17 + symbol_count;
        }
    }
    Err(invalid_jpeg("standard Huffman table has no zero symbol"))
}

fn append_zero_entropy(
    output: &mut Vec<u8>,
    coefficients: &CoefficientImage,
    huffman_segments: &[Vec<u8>],
    restart_interval: usize,
) -> Result<()> {
    let luma_dc = zero_code(huffman_segments, 0x00)?;
    let luma_ac = zero_code(huffman_segments, 0x10)?;
    let chroma_dc = zero_code(huffman_segments, 0x01)?;
    let chroma_ac = zero_code(huffman_segments, 0x11)?;
    let first = coefficients
        .components
        .first()
        .ok_or_else(|| invalid_jpeg("coefficient image has no components"))?;
    let mcu_columns = first.blocks_wide / first.horizontal_sampling;
    let mcu_rows = first.blocks_high / first.vertical_sampling;
    let mut writer = SkeletonBitWriter::new(output);
    let mut mcu_index = 0_usize;
    let mut restart_index = 0_u8;
    for _ in 0..mcu_rows {
        for _ in 0..mcu_columns {
            if restart_interval > 0 && mcu_index > 0 && mcu_index.is_multiple_of(restart_interval) {
                writer.restart(restart_index);
                restart_index = (restart_index + 1) & 7;
            }
            for component in &coefficients.components {
                let block_count = component.horizontal_sampling * component.vertical_sampling;
                let (dc, ac) = if component.luma {
                    (luma_dc, luma_ac)
                } else {
                    (chroma_dc, chroma_ac)
                };
                for _ in 0..block_count {
                    writer.write(dc);
                    writer.write(ac);
                }
            }
            mcu_index += 1;
        }
    }
    writer.finish();
    Ok(())
}

struct SkeletonBitWriter<'a> {
    output: &'a mut Vec<u8>,
    current: u8,
    used: u8,
}

impl<'a> SkeletonBitWriter<'a> {
    fn new(output: &'a mut Vec<u8>) -> Self {
        Self {
            output,
            current: 0,
            used: 0,
        }
    }

    fn write(&mut self, code: EncodeCode) {
        for shift in (0..code.length).rev() {
            self.current = (self.current << 1) | ((code.bits >> shift) as u8 & 1);
            self.used += 1;
            if self.used == 8 {
                self.flush_byte();
            }
        }
    }

    fn restart(&mut self, index: u8) {
        self.pad();
        self.output.extend([0xff, 0xd0 + index]);
    }

    fn finish(&mut self) {
        self.pad();
    }

    fn pad(&mut self) {
        if self.used > 0 {
            self.current = (self.current << (8 - self.used)) | ((1_u8 << (8 - self.used)) - 1);
            self.flush_byte();
        }
    }

    fn flush_byte(&mut self) {
        self.output.push(self.current);
        if self.current == 0xff {
            self.output.push(0);
        }
        self.current = 0;
        self.used = 0;
    }
}

fn append_segment(output: &mut Vec<u8>, marker: u8, payload: &[u8]) -> Result<()> {
    let length = u16::try_from(payload.len() + 2)
        .map_err(|_| invalid_jpeg("segment exceeds JPEG length limit"))?;
    output.extend([0xff, marker]);
    output.extend(length.to_be_bytes());
    output.extend_from_slice(payload);
    Ok(())
}

fn mutate_dht_segment(
    segment: &mut [u8],
    swaps: usize,
    options: HuffmanGlitchOptions,
    usage: Option<&AcUsage>,
    rng: &mut SeededRng,
) -> Result<usize> {
    let mut offset = 0;
    let mut tables_found = 0;
    while offset < segment.len() {
        if segment.len() - offset < 17 {
            return Err(invalid_jpeg("truncated Huffman table"));
        }
        let table_class = segment[offset] >> 4;
        let table_identifier = usize::from(segment[offset] & 0x0f);
        if table_identifier >= 4 {
            return Err(invalid_jpeg("Huffman table identifier exceeds JPEG limits"));
        }
        let symbol_count: usize = segment[offset + 1..offset + 17]
            .iter()
            .map(|count| usize::from(*count))
            .sum();
        let symbols_start = offset + 17;
        let symbols_end = symbols_start
            .checked_add(symbol_count)
            .filter(|end| *end <= segment.len())
            .ok_or_else(|| invalid_jpeg("truncated Huffman symbols"))?;

        if table_class == 1 {
            tables_found += 1;
        }
        if table_class == 1 && target_matches(table_identifier, options.target, usage) {
            let symbols = &mut segment[symbols_start..symbols_end];
            for _ in 0..swaps {
                let counts = usage.map(|usage| &usage.tables[table_identifier].counts);
                let Some((first, second)) = select_pair(symbols, counts, options, rng) else {
                    continue;
                };
                symbols.swap(first, second);
            }
        }
        offset = symbols_end;
    }
    Ok(tables_found)
}

fn target_matches(table: usize, target: HuffmanTarget, usage: Option<&AcUsage>) -> bool {
    match target {
        HuffmanTarget::All => true,
        HuffmanTarget::LumaAc => usage.map_or(table == 0, |usage| usage.tables[table].luma),
        HuffmanTarget::ChromaAc => usage.map_or(table != 0, |usage| usage.tables[table].chroma),
    }
}

#[derive(Clone, Copy, Debug)]
struct SymbolPair {
    first: usize,
    second: usize,
    impact: u64,
}

fn select_pair(
    symbols: &[u8],
    counts: Option<&[u64; 256]>,
    options: HuffmanGlitchOptions,
    rng: &mut SeededRng,
) -> Option<(usize, usize)> {
    let preserve_size =
        options.preserve_size || matches!(options.mode, HuffmanGlitchMode::RunRemap);
    let max_run_delta = ((options.intensity * 15.0).ceil() as u8).max(1);
    let mut pairs = Vec::new();
    for first in 0..symbols.len() {
        let first_symbol = symbols[first];
        if matches!(first_symbol, 0x00 | 0xf0) {
            continue;
        }
        for (second, second_symbol) in symbols.iter().copied().enumerate().skip(first + 1) {
            if matches!(second_symbol, 0x00 | 0xf0) {
                continue;
            }
            if preserve_size && (first_symbol & 0x0f) != (second_symbol & 0x0f) {
                continue;
            }
            if matches!(options.mode, HuffmanGlitchMode::RunRemap) {
                let run_delta = (first_symbol >> 4).abs_diff(second_symbol >> 4);
                if run_delta == 0 || run_delta > max_run_delta {
                    continue;
                }
            }
            let impact = counts.map_or(0, |counts| {
                counts[usize::from(first_symbol)].saturating_add(counts[usize::from(second_symbol)])
            });
            pairs.push(SymbolPair {
                first,
                second,
                impact,
            });
        }
    }
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by_key(|pair| pair.impact);
    let pool_fraction = options.intensity.powi(2);
    let pool_length = ((pairs.len() as f64 * pool_fraction).ceil() as usize)
        .max(1)
        .min(pairs.len());
    let pair = pairs[rng.index(pool_length)];
    Some((pair.first, pair.second))
}

struct SeededRng(u64);

impl SeededRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }

    fn index(&mut self, length: usize) -> usize {
        (self.next() as usize) % length
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataSegment {
    pub(crate) marker: u8,
    pub(crate) data: Vec<u8>,
}

pub(crate) fn extract_metadata(input: &[u8]) -> Result<Vec<MetadataSegment>> {
    if !input.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("missing SOI marker"));
    }

    let mut metadata = Vec::new();
    let mut offset = 2;
    while offset < input.len() {
        if input[offset] != 0xff {
            return Err(invalid_jpeg("expected marker prefix before scan data"));
        }
        while offset < input.len() && input[offset] == 0xff {
            offset += 1;
        }
        if offset >= input.len() {
            return Err(invalid_jpeg("truncated marker"));
        }
        let marker = input[offset];
        offset += 1;

        if marker == 0xda {
            return Ok(metadata);
        }
        if marker == 0xd9 {
            return Err(invalid_jpeg("EOI appeared before SOS"));
        }
        if marker == 0x00 || marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            return Err(invalid_jpeg(format!(
                "unexpected standalone marker ff{marker:02x} before SOS"
            )));
        }
        if input.len() - offset < 2 {
            return Err(invalid_jpeg("truncated segment length"));
        }
        let length = usize::from(u16::from_be_bytes([input[offset], input[offset + 1]]));
        if length < 2 {
            return Err(invalid_jpeg("segment length is smaller than its header"));
        }
        let data_start = offset + 2;
        let data_end = offset
            .checked_add(length)
            .filter(|end| *end <= input.len())
            .ok_or_else(|| invalid_jpeg("segment length exceeds file size"))?;
        if matches!(marker, 0xe1 | 0xe2 | 0xfe) {
            metadata.push(MetadataSegment {
                marker,
                data: input[data_start..data_end].to_vec(),
            });
        }
        offset = data_end;
    }

    Err(invalid_jpeg("missing SOS marker"))
}

pub(crate) fn inject_metadata(encoded: &[u8], metadata: &[MetadataSegment]) -> Result<Vec<u8>> {
    if !encoded.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("encoded image is missing SOI marker"));
    }
    let extra_length = metadata.iter().try_fold(0_usize, |total, segment| {
        total
            .checked_add(segment.data.len() + 4)
            .ok_or_else(|| invalid_jpeg("metadata size overflow"))
    })?;
    let mut output = Vec::with_capacity(encoded.len() + extra_length);
    output.extend_from_slice(&encoded[..2]);
    for segment in metadata {
        let length = u16::try_from(segment.data.len() + 2)
            .map_err(|_| invalid_jpeg("metadata segment exceeds JPEG length limit"))?;
        output.extend_from_slice(&[0xff, segment.marker]);
        output.extend_from_slice(&length.to_be_bytes());
        output.extend_from_slice(&segment.data);
    }
    output.extend_from_slice(&encoded[2..]);
    Ok(output)
}

fn invalid_jpeg(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid JPEG: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgb};
    use sha2::{Digest, Sha256};

    use super::*;

    fn fixture() -> Vec<u8> {
        let image = ImageBuffer::from_pixel(2, 2, Rgb([20_u8, 40, 60]));
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&image)
            .unwrap();
        encoded
    }

    fn options(swaps: usize) -> HuffmanGlitchOptions {
        HuffmanGlitchOptions {
            swaps,
            intensity: 1.0,
            target: HuffmanTarget::All,
            mode: HuffmanGlitchMode::RunRemap,
            preserve_size: true,
            scan_start: 0,
            scan_count: 0,
            frequency_start: 1,
            frequency_end: 63,
        }
    }

    fn textured_fixture() -> Vec<u8> {
        let image = ImageBuffer::from_fn(32, 24, |x, y| {
            Rgb([(x * 7) as u8, (y * 11) as u8, ((x + y) * 5) as u8])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&image)
            .unwrap();
        encoded
    }

    fn ac_symbols(input: &[u8], identifier: u8) -> Vec<u8> {
        let mut offset = 2;
        while offset + 4 <= input.len() {
            assert_eq!(input[offset], 0xff);
            let marker = input[offset + 1];
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            let length = usize::from(u16::from_be_bytes([input[offset + 2], input[offset + 3]]));
            let end = offset + 2 + length;
            if marker == 0xc4 {
                let mut table = offset + 4;
                while table < end {
                    let selector = input[table];
                    let count = input[table + 1..table + 17]
                        .iter()
                        .map(|count| usize::from(*count))
                        .sum::<usize>();
                    if selector == 0x10 | identifier {
                        return input[table + 17..table + 17 + count].to_vec();
                    }
                    table += 17 + count;
                }
            }
            offset = end;
        }
        panic!("missing AC Huffman table {identifier}");
    }

    #[test]
    fn extracts_and_reinjects_supported_metadata() {
        let metadata = vec![
            MetadataSegment {
                marker: 0xe1,
                data: b"Exif\0\0fixture".to_vec(),
            },
            MetadataSegment {
                marker: 0xe2,
                data: b"ICC_PROFILE\0fixture".to_vec(),
            },
            MetadataSegment {
                marker: 0xfe,
                data: b"Databender".to_vec(),
            },
        ];

        let output = inject_metadata(&fixture(), &metadata).unwrap();

        assert_eq!(extract_metadata(&output).unwrap(), metadata);
        assert!(image::load_from_memory(&output).is_ok());
    }

    #[test]
    fn rejects_truncated_segments() {
        assert!(extract_metadata(&[0xff, 0xd8, 0xff, 0xe1, 0x00, 0x20]).is_err());
    }

    #[test]
    fn mutates_huffman_tables_deterministically_without_changing_structure() {
        let input = fixture();

        let first = mutate_huffman_tables(&input, options(8), 42).unwrap();
        let second = mutate_huffman_tables(&input, options(8), 42).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, input);
        assert_eq!(first.len(), input.len());
        assert!(image::load_from_memory(&first).is_ok());
    }

    #[test]
    fn mutates_coefficients_deterministically_and_reencodes_valid_jpeg() {
        let input = textured_fixture();
        let mut coefficient_options = options(16);
        coefficient_options.mode = HuffmanGlitchMode::SymbolRemap;

        let first = mutate_coefficients(&input, coefficient_options, 42).unwrap();
        let second = mutate_coefficients(&input, coefficient_options, 42).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, input);
        assert!(image::load_from_memory(&first).is_ok());
        assert_eq!(
            extract_metadata(&first).unwrap(),
            extract_metadata(&input).unwrap()
        );
    }

    #[test]
    fn reconstructs_and_reencodes_progressive_coefficients() {
        let input = crate::codecs::jpeg_entropy::tests::progressive_fixture();
        let mut coefficient_options = options(1);
        coefficient_options.mode = HuffmanGlitchMode::SymbolRemap;
        coefficient_options.scan_start = 3;
        coefficient_options.scan_count = 1;
        coefficient_options.frequency_start = 1;
        coefficient_options.frequency_end = 1;

        let output = mutate_coefficients(&input, coefficient_options, 42).unwrap();
        let output_coefficients = dct_io::read_coefficients(&output).unwrap();

        assert!(image::load_from_memory(&output).is_ok());
        assert!(!has_frame_marker(&output, 0xc2).unwrap());
        assert_eq!(output_coefficients.components[0].blocks[0][0], 3);
        assert_eq!(output_coefficients.components[0].blocks[0][1], -3);
    }

    #[test]
    fn preserves_progressive_restart_interval_when_reencoding() {
        let input = crate::codecs::jpeg_entropy::tests::progressive_restart_fixture();
        let mut coefficient_options = options(1);
        coefficient_options.mode = HuffmanGlitchMode::SymbolRemap;
        coefficient_options.scan_start = 3;
        coefficient_options.scan_count = 1;
        coefficient_options.frequency_start = 1;
        coefficient_options.frequency_end = 1;

        let output = mutate_coefficients(&input, coefficient_options, 42).unwrap();

        assert!(output
            .windows(6)
            .any(|bytes| bytes == [0xff, 0xdd, 0, 4, 0, 1]));
        assert!(output
            .windows(2)
            .any(|bytes| bytes[0] == 0xff && (0xd0..=0xd7).contains(&bytes[1])));
        image::load_from_memory(&output).unwrap();
    }

    #[test]
    fn progressive_coefficient_output_matches_golden_digest() {
        let input = crate::codecs::jpeg_entropy::tests::progressive_fixture();
        let mut coefficient_options = options(1);
        coefficient_options.mode = HuffmanGlitchMode::SymbolRemap;
        coefficient_options.scan_start = 3;
        coefficient_options.scan_count = 1;
        coefficient_options.frequency_start = 1;
        coefficient_options.frequency_end = 1;

        let output = mutate_coefficients(&input, coefficient_options, 42).unwrap();
        let digest: [u8; 32] = Sha256::digest(output).into();

        assert_eq!(
            digest,
            [
                255, 221, 224, 172, 14, 74, 114, 3, 50, 96, 144, 173, 55, 169, 189, 56, 252, 198,
                65, 89, 177, 73, 28, 31, 183, 247, 125, 118, 55, 206, 132, 200,
            ]
        );
    }

    #[test]
    fn finds_huffman_tables_after_scan_data() {
        let mut input = vec![0xff, 0xd8, 0xff, 0xda, 0x00, 0x02];
        input.extend_from_slice(&[0x12, 0xff, 0x00, 0x34, 0xff, 0xd0]);
        input.extend_from_slice(&[
            0xff, 0xc4, 0x00, 0x15, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x11, 0xff, 0xd9,
        ]);

        let output = mutate_huffman_tables(&input, options(1), 42).unwrap();

        assert_ne!(output, input);
        assert_eq!(output.len(), input.len());
    }

    #[test]
    fn zero_intensity_preserves_the_input() {
        let input = fixture();
        let mut options = options(128);
        options.intensity = 0.0;

        assert_eq!(mutate_huffman_tables(&input, options, 42).unwrap(), input);
    }

    #[test]
    fn luma_target_leaves_chroma_table_untouched() {
        let input = textured_fixture();
        let mut options = options(16);
        options.target = HuffmanTarget::LumaAc;

        let output = mutate_huffman_tables(&input, options, 42).unwrap();

        assert_ne!(ac_symbols(&output, 0), ac_symbols(&input, 0));
        assert_eq!(ac_symbols(&output, 1), ac_symbols(&input, 1));
        assert!(image::load_from_memory(&output).is_ok());
    }

    #[test]
    fn low_intensity_selects_the_lowest_impact_nearby_run_pair() {
        let symbols = [0x01, 0x11, 0x21, 0x31];
        let mut counts = [0_u64; 256];
        counts[0x01] = 1_000;
        counts[0x11] = 500;
        counts[0x21] = 1;
        counts[0x31] = 2;
        let mut options = options(32);
        options.intensity = 0.2;

        let pair = select_pair(&symbols, Some(&counts), options, &mut SeededRng::new(42));

        assert_eq!(pair, Some((2, 3)));
    }

    #[test]
    fn intensity_scales_the_number_of_changed_table_entries() {
        let input = textured_fixture();
        let mut low_options = options(32);
        low_options.intensity = 0.2;
        low_options.target = HuffmanTarget::LumaAc;
        let mut high_options = low_options;
        high_options.intensity = 1.0;

        let low = mutate_huffman_tables(&input, low_options, 42).unwrap();
        let high = mutate_huffman_tables(&input, high_options, 42).unwrap();
        let original_symbols = ac_symbols(&input, 0);
        let low_changes = original_symbols
            .iter()
            .zip(ac_symbols(&low, 0))
            .filter(|(before, after)| **before != *after)
            .count();
        let high_changes = original_symbols
            .iter()
            .zip(ac_symbols(&high, 0))
            .filter(|(before, after)| **before != *after)
            .count();

        assert_eq!(low_changes, 2);
        assert!(high_changes > low_changes);
    }
}
