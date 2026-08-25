use crate::{
    codecs::jpeg_entropy::{analyze_ac_usage, AcUsage},
    DatabenderError, HuffmanGlitchMode, HuffmanTarget, Result,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct HuffmanGlitchOptions {
    pub(crate) swaps: usize,
    pub(crate) intensity: f64,
    pub(crate) target: HuffmanTarget,
    pub(crate) mode: HuffmanGlitchMode,
    pub(crate) preserve_size: bool,
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
