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

#[derive(Clone, Debug)]
pub(crate) struct CoefficientImage {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) components: Vec<ComponentCoefficients>,
}

#[derive(Clone, Debug)]
pub(crate) struct ComponentCoefficients {
    pub(crate) id: u8,
    pub(crate) luma: bool,
    pub(crate) horizontal_sampling: usize,
    pub(crate) vertical_sampling: usize,
    pub(crate) blocks_wide: usize,
    pub(crate) blocks_high: usize,
    pub(crate) blocks: Vec<[i16; 64]>,
    pub(crate) scans: Vec<[u16; 64]>,
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
                let _ = decode_scan(
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

pub(crate) fn reconstruct_coefficients(input: &[u8]) -> Result<CoefficientImage> {
    if !input.starts_with(&[0xff, 0xd8]) {
        return Err(invalid_jpeg("missing SOI marker"));
    }

    let mut offset = 2;
    let mut frame = None;
    let mut dc_tables: [Option<HuffmanTable>; 4] = array::from_fn(|_| None);
    let mut ac_tables: [Option<HuffmanTable>; 4] = array::from_fn(|_| None);
    let mut restart_interval = 0_usize;
    let mut usage = AcUsage::default();
    let mut blocks: Option<Vec<Vec<[i16; 64]>>> = None;
    let mut scans: Option<Vec<Vec<[u16; 64]>>> = None;
    let mut progressive = false;
    let mut saw_scan = false;
    let mut scan_index = 0_u16;

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
            0xc0 => {
                let parsed = parse_frame(segment)?;
                blocks = Some(vec![Vec::new(); parsed.components.len()]);
                scans = Some(vec![Vec::new(); parsed.components.len()]);
                frame = Some(parsed);
            }
            0xc2 => {
                let parsed = parse_frame(segment)?;
                blocks = Some(allocate_coefficient_blocks(&parsed));
                scans = Some(allocate_scan_origins(&parsed));
                frame = Some(parsed);
                progressive = true;
            }
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
                let entropy_end = find_entropy_end(input, segment_end)?;
                let blocks = blocks
                    .as_mut()
                    .ok_or_else(|| invalid_jpeg("missing coefficient storage"))?;
                let scans = scans
                    .as_mut()
                    .ok_or_else(|| invalid_jpeg("missing scan provenance storage"))?;
                if progressive {
                    let scan = parse_progressive_scan(segment, frame)?;
                    decode_progressive_scan(
                        &input[segment_end..entropy_end],
                        frame,
                        &scan,
                        &dc_tables,
                        &ac_tables,
                        restart_interval,
                        blocks,
                        scans,
                        scan_index,
                    )?;
                } else {
                    let scan = parse_scan(segment, frame, &mut usage)?;
                    let decoded = decode_scan(
                        &input[segment_end..entropy_end],
                        frame,
                        &scan,
                        &dc_tables,
                        &ac_tables,
                        restart_interval,
                        &mut usage,
                    )?;
                    for (component, block) in decoded {
                        blocks[component].push(block);
                        scans[component].push([scan_index; 64]);
                    }
                }
                saw_scan = true;
                scan_index = scan_index
                    .checked_add(1)
                    .ok_or_else(|| invalid_jpeg("too many JPEG scans"))?;
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
    let frame = frame.ok_or_else(|| invalid_jpeg("missing baseline frame"))?;
    let blocks = blocks.ok_or_else(|| invalid_jpeg("missing coefficient storage"))?;
    let scans = scans.ok_or_else(|| invalid_jpeg("missing scan provenance storage"))?;
    let (mcu_columns, mcu_rows) = frame_mcu_geometry(&frame);
    let components = frame
        .components
        .iter()
        .zip(blocks.into_iter().zip(scans))
        .map(|(component, (blocks, scans))| ComponentCoefficients {
            id: component.id,
            luma: component.luma,
            horizontal_sampling: component.horizontal,
            vertical_sampling: component.vertical,
            blocks_wide: mcu_columns * component.horizontal,
            blocks_high: mcu_rows * component.vertical,
            blocks,
            scans,
        })
        .collect();
    Ok(CoefficientImage {
        width: frame.width,
        height: frame.height,
        components,
    })
}

fn frame_mcu_geometry(frame: &Frame) -> (usize, usize) {
    (
        divide_round_up(frame.width, 8 * frame.max_horizontal),
        divide_round_up(frame.height, 8 * frame.max_vertical),
    )
}

fn allocate_coefficient_blocks(frame: &Frame) -> Vec<Vec<[i16; 64]>> {
    let (mcu_columns, mcu_rows) = frame_mcu_geometry(frame);
    frame
        .components
        .iter()
        .map(|component| {
            vec![[0_i16; 64]; mcu_columns * component.horizontal * mcu_rows * component.vertical]
        })
        .collect()
}

fn allocate_scan_origins(frame: &Frame) -> Vec<Vec<[u16; 64]>> {
    allocate_coefficient_blocks(frame)
        .into_iter()
        .map(|blocks| vec![[u16::MAX; 64]; blocks.len()])
        .collect()
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

#[derive(Clone, Debug)]
struct ProgressiveScan {
    components: Vec<ScanComponent>,
    spectral_start: usize,
    spectral_end: usize,
    approximation_high: u8,
    approximation_low: u8,
}

fn parse_progressive_scan(segment: &[u8], frame: &Frame) -> Result<ProgressiveScan> {
    let component_count = segment.first().copied().map(usize::from).unwrap_or(0);
    let parameters_start = 1 + component_count * 2;
    if component_count == 0 || segment.len() != parameters_start + 3 {
        return Err(invalid_jpeg("invalid progressive SOS component list"));
    }
    let spectral_start = usize::from(segment[parameters_start]);
    let spectral_end = usize::from(segment[parameters_start + 1]);
    let approximation_high = segment[parameters_start + 2] >> 4;
    let approximation_low = segment[parameters_start + 2] & 0x0f;
    if spectral_start > spectral_end || spectral_end > 63 {
        return Err(invalid_jpeg("invalid progressive spectral selection"));
    }
    if spectral_start == 0 && spectral_end != 0 {
        return Err(invalid_jpeg(
            "progressive DC scan must end at coefficient zero",
        ));
    }
    if spectral_start > 0 && component_count != 1 {
        return Err(invalid_jpeg("progressive AC scans must have one component"));
    }
    if approximation_high > 13 || approximation_low > 13 {
        return Err(invalid_jpeg(
            "progressive approximation exceeds JPEG limits",
        ));
    }
    if approximation_high > 0 && approximation_high != approximation_low + 1 {
        return Err(invalid_jpeg("invalid progressive refinement step"));
    }

    let mut components = Vec::with_capacity(component_count);
    for index in 0..component_count {
        let start = 1 + index * 2;
        let frame_index = frame
            .components
            .iter()
            .position(|component| component.id == segment[start])
            .ok_or_else(|| invalid_jpeg("progressive SOS references an unknown component"))?;
        let selectors = segment[start + 1];
        let dc_table = usize::from(selectors >> 4);
        let ac_table = usize::from(selectors & 0x0f);
        if dc_table >= 4 || ac_table >= 4 {
            return Err(invalid_jpeg("Huffman table selector exceeds JPEG limits"));
        }
        components.push(ScanComponent {
            frame_index,
            dc_table,
            ac_table,
        });
    }
    Ok(ProgressiveScan {
        components,
        spectral_start,
        spectral_end,
        approximation_high,
        approximation_low,
    })
}

#[allow(clippy::too_many_arguments)]
fn decode_progressive_scan(
    entropy: &[u8],
    frame: &Frame,
    scan: &ProgressiveScan,
    dc_tables: &[Option<HuffmanTable>; 4],
    ac_tables: &[Option<HuffmanTable>; 4],
    restart_interval: usize,
    blocks: &mut [Vec<[i16; 64]>],
    scans: &mut [Vec<[u16; 64]>],
    scan_index: u16,
) -> Result<()> {
    let is_dc = scan.spectral_start == 0;
    let interleaved = is_dc && scan.components.len() > 1;
    let (mcu_columns, mcu_rows) = frame_mcu_geometry(frame);
    let (scan_columns, scan_rows) = if interleaved {
        (mcu_columns, mcu_rows)
    } else {
        let component = &frame.components[scan.components[0].frame_index];
        (
            mcu_columns * component.horizontal,
            mcu_rows * component.vertical,
        )
    };
    let mut reader = EntropyReader::new(entropy);
    let mut predictors = vec![0_i16; frame.components.len()];
    let mut eob_run = 0_u32;
    let mut mcu_index = 0_usize;

    for row in 0..scan_rows {
        for column in 0..scan_columns {
            if restart_interval > 0 && mcu_index > 0 && mcu_index.is_multiple_of(restart_interval) {
                reader.align_to_byte();
                predictors.fill(0);
                eob_run = 0;
            }
            if is_dc && interleaved {
                for scan_component in &scan.components {
                    let component = &frame.components[scan_component.frame_index];
                    let blocks_wide = mcu_columns * component.horizontal;
                    for block_y in 0..component.vertical {
                        for block_x in 0..component.horizontal {
                            let block_index = (row * component.vertical + block_y) * blocks_wide
                                + column * component.horizontal
                                + block_x;
                            decode_progressive_dc(
                                &mut reader,
                                dc_tables[scan_component.dc_table]
                                    .as_ref()
                                    .ok_or_else(|| invalid_jpeg("missing progressive DC table"))?,
                                &mut predictors[scan_component.frame_index],
                                &mut blocks[scan_component.frame_index][block_index],
                                scan.approximation_high,
                                scan.approximation_low,
                                &mut scans[scan_component.frame_index][block_index],
                                scan_index,
                            )?;
                        }
                    }
                }
            } else {
                let scan_component = &scan.components[0];
                let block_index = row * scan_columns + column;
                let block = &mut blocks[scan_component.frame_index][block_index];
                if is_dc {
                    decode_progressive_dc(
                        &mut reader,
                        dc_tables[scan_component.dc_table]
                            .as_ref()
                            .ok_or_else(|| invalid_jpeg("missing progressive DC table"))?,
                        &mut predictors[scan_component.frame_index],
                        block,
                        scan.approximation_high,
                        scan.approximation_low,
                        &mut scans[scan_component.frame_index][block_index],
                        scan_index,
                    )?;
                } else {
                    let table = ac_tables[scan_component.ac_table]
                        .as_ref()
                        .ok_or_else(|| invalid_jpeg("missing progressive AC table"))?;
                    if scan.approximation_high == 0 {
                        decode_progressive_ac_first(
                            &mut reader,
                            table,
                            block,
                            scan.spectral_start,
                            scan.spectral_end,
                            scan.approximation_low,
                            &mut eob_run,
                            &mut scans[scan_component.frame_index][block_index],
                            scan_index,
                        )?;
                    } else {
                        decode_progressive_ac_refinement(
                            &mut reader,
                            table,
                            block,
                            scan.spectral_start,
                            scan.spectral_end,
                            scan.approximation_low,
                            &mut eob_run,
                            &mut scans[scan_component.frame_index][block_index],
                            scan_index,
                        )?;
                    }
                }
            }
            mcu_index += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_progressive_dc(
    reader: &mut EntropyReader<'_>,
    table: &HuffmanTable,
    predictor: &mut i16,
    block: &mut [i16; 64],
    approximation_high: u8,
    approximation_low: u8,
    scans: &mut [u16; 64],
    scan_index: u16,
) -> Result<()> {
    if approximation_high == 0 {
        let size = table.decode(reader)?;
        if size > 11 {
            return Err(invalid_jpeg("invalid progressive DC coefficient size"));
        }
        let difference = receive_extend(reader.read_bits(size)?, size)?;
        *predictor = predictor
            .checked_add(difference)
            .ok_or_else(|| invalid_jpeg("progressive DC coefficient overflows i16"))?;
        block[0] = predictor
            .checked_shl(u32::from(approximation_low))
            .ok_or_else(|| invalid_jpeg("progressive DC shift overflows i16"))?;
        scans[0] = scan_index;
    } else if reader.read_bit()? != 0 {
        block[0] |= 1_i16 << approximation_low;
        scans[0] = scan_index;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_progressive_ac_first(
    reader: &mut EntropyReader<'_>,
    table: &HuffmanTable,
    block: &mut [i16; 64],
    spectral_start: usize,
    spectral_end: usize,
    approximation_low: u8,
    eob_run: &mut u32,
    scans: &mut [u16; 64],
    scan_index: u16,
) -> Result<()> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Ok(());
    }
    let mut coefficient = spectral_start;
    while coefficient <= spectral_end {
        let symbol = table.decode(reader)?;
        let run = usize::from(symbol >> 4);
        let size = symbol & 0x0f;
        if size == 0 {
            if run == 15 {
                coefficient += 16;
                continue;
            }
            let extra = u32::from(reader.read_bits(run as u8)?);
            *eob_run = (1_u32 << run) + extra - 1;
            return Ok(());
        }
        if size > 10 {
            return Err(invalid_jpeg("invalid progressive AC coefficient size"));
        }
        coefficient += run;
        if coefficient > spectral_end {
            return Err(invalid_jpeg("progressive AC run exceeds spectral band"));
        }
        let value = receive_extend(reader.read_bits(size)?, size)?;
        block[coefficient] = value
            .checked_shl(u32::from(approximation_low))
            .ok_or_else(|| invalid_jpeg("progressive AC shift overflows i16"))?;
        scans[coefficient] = scan_index;
        coefficient += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_progressive_ac_refinement(
    reader: &mut EntropyReader<'_>,
    table: &HuffmanTable,
    block: &mut [i16; 64],
    spectral_start: usize,
    spectral_end: usize,
    approximation_low: u8,
    eob_run: &mut u32,
    scans: &mut [u16; 64],
    scan_index: u16,
) -> Result<()> {
    let bit = 1_i16 << approximation_low;
    let mut coefficient = spectral_start;
    if *eob_run == 0 {
        while coefficient <= spectral_end {
            let symbol = table.decode(reader)?;
            let mut run = usize::from(symbol >> 4);
            let size = symbol & 0x0f;
            let new_value = if size == 0 {
                if run != 15 {
                    let extra = u32::from(reader.read_bits(run as u8)?);
                    *eob_run = (1_u32 << run) + extra;
                    break;
                }
                0
            } else if size == 1 {
                if reader.read_bit()? == 0 {
                    -bit
                } else {
                    bit
                }
            } else {
                return Err(invalid_jpeg("invalid progressive AC refinement symbol"));
            };

            loop {
                if coefficient > spectral_end {
                    return Err(invalid_jpeg("progressive AC refinement exceeds band"));
                }
                if block[coefficient] != 0 {
                    if refine_nonzero(reader, &mut block[coefficient], bit)? {
                        scans[coefficient] = scan_index;
                    }
                } else if run == 0 {
                    break;
                } else {
                    run -= 1;
                }
                coefficient += 1;
            }
            if new_value != 0 {
                block[coefficient] = new_value;
                scans[coefficient] = scan_index;
            }
            coefficient += 1;
        }
    }
    if *eob_run > 0 {
        while coefficient <= spectral_end {
            if block[coefficient] != 0 && refine_nonzero(reader, &mut block[coefficient], bit)? {
                scans[coefficient] = scan_index;
            }
            coefficient += 1;
        }
        *eob_run -= 1;
    }
    Ok(())
}

fn refine_nonzero(reader: &mut EntropyReader<'_>, coefficient: &mut i16, bit: i16) -> Result<bool> {
    if reader.read_bit()? != 0 && (*coefficient & bit) == 0 {
        if *coefficient >= 0 {
            *coefficient += bit;
        } else {
            *coefficient -= bit;
        }
        return Ok(true);
    }
    Ok(false)
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
) -> Result<Vec<(usize, [i16; 64])>> {
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
    let mut predictors = vec![0_i16; frame.components.len()];
    let mut decoded = Vec::new();

    for mcu_index in 0..mcu_count {
        if restart_interval > 0 && mcu_index > 0 && mcu_index % restart_interval == 0 {
            reader.align_to_byte();
            predictors.fill(0);
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
                let block = decode_block(
                    &mut reader,
                    dc_table,
                    ac_table,
                    &mut usage.tables[scan_component.ac_table].counts,
                    &mut predictors[scan_component.frame_index],
                )?;
                decoded.push((scan_component.frame_index, block));
            }
        }
    }
    Ok(decoded)
}

fn decode_block(
    reader: &mut EntropyReader<'_>,
    dc_table: &HuffmanTable,
    ac_table: &HuffmanTable,
    counts: &mut [u64; 256],
    dc_predictor: &mut i16,
) -> Result<[i16; 64]> {
    let mut block = [0_i16; 64];
    let dc_size = dc_table.decode(reader)?;
    if dc_size > 11 {
        return Err(invalid_jpeg("invalid baseline DC coefficient size"));
    }
    let dc_difference = receive_extend(reader.read_bits(dc_size)?, dc_size)?;
    *dc_predictor = dc_predictor
        .checked_add(dc_difference)
        .ok_or_else(|| invalid_jpeg("DC coefficient overflows i16"))?;
    block[0] = *dc_predictor;

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
        block[coefficient] = receive_extend(reader.read_bits(size)?, size)?;
        coefficient += 1;
    }
    Ok(block)
}

fn receive_extend(bits: u16, size: u8) -> Result<i16> {
    if size == 0 {
        return Ok(0);
    }
    let threshold = 1_u16 << (size - 1);
    if bits >= threshold {
        i16::try_from(bits).map_err(|_| invalid_jpeg("coefficient magnitude overflows i16"))
    } else {
        let extended = i32::from(bits) - ((1_i32 << size) - 1);
        i16::try_from(extended).map_err(|_| invalid_jpeg("coefficient magnitude overflows i16"))
    }
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

    fn read_bits(&mut self, count: u8) -> Result<u16> {
        let mut value = 0_u16;
        for _ in 0..count {
            value = (value << 1) | u16::from(self.read_bit()?);
        }
        Ok(value)
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
pub(crate) mod tests {
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
    fn reconstructs_signed_quantized_coefficient_blocks() {
        let image = ImageBuffer::from_fn(32, 24, |x, y| {
            Rgb([(x * 7) as u8, (y * 11) as u8, ((x + y) * 5) as u8])
        });
        let mut encoded = Vec::new();
        JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&image)
            .unwrap();

        let coefficients = reconstruct_coefficients(&encoded).unwrap();

        assert_eq!((coefficients.width, coefficients.height), (32, 24));
        assert_eq!(coefficients.components.len(), 3);
        assert!(coefficients.components[0].luma);
        assert_eq!(coefficients.components[0].id, 1);
        assert!(coefficients
            .components
            .iter()
            .all(|component| !component.blocks.is_empty()));
        assert!(coefficients
            .components
            .iter()
            .flat_map(|component| &component.blocks)
            .flatten()
            .any(|coefficient| *coefficient < 0));
        assert!(coefficients
            .components
            .iter()
            .flat_map(|component| &component.blocks)
            .flatten()
            .any(|coefficient| *coefficient > 0));
    }

    #[test]
    fn reconstructs_progressive_first_and_refinement_scans() {
        let progressive = progressive_fixture();

        let coefficients = reconstruct_coefficients(&progressive).unwrap();

        assert_eq!((coefficients.width, coefficients.height), (8, 8));
        assert_eq!(coefficients.components.len(), 1);
        assert_eq!(coefficients.components[0].horizontal_sampling, 1);
        assert_eq!(coefficients.components[0].vertical_sampling, 1);
        assert_eq!(coefficients.components[0].blocks_wide, 1);
        assert_eq!(coefficients.components[0].blocks_high, 1);
        assert_eq!(coefficients.components[0].blocks[0][0], 3);
        assert_eq!(coefficients.components[0].blocks[0][1], 3);
        image::load_from_memory(&progressive).unwrap();
    }

    #[test]
    fn reconstructs_progressive_scans_across_restart_intervals() {
        let progressive = progressive_restart_fixture();

        let coefficients = reconstruct_coefficients(&progressive).unwrap();

        assert_eq!(coefficients.components[0].blocks.len(), 2);
        assert!(coefficients.components[0]
            .blocks
            .iter()
            .all(|block| block[0] == 3 && block[1] == 3));
        image::load_from_memory(&progressive).unwrap();
    }

    #[test]
    fn declines_progressive_entropy_analysis() {
        let progressive = [
            0xff, 0xd8, 0xff, 0xc2, 0x00, 0x08, 8, 0, 1, 0, 1, 0, 0xff, 0xd9,
        ];

        assert!(analyze_ac_usage(&progressive).unwrap().is_none());
    }

    pub(crate) fn progressive_fixture() -> Vec<u8> {
        build_progressive_fixture(8, false)
    }

    pub(crate) fn progressive_restart_fixture() -> Vec<u8> {
        build_progressive_fixture(16, true)
    }

    fn build_progressive_fixture(width: u16, restart: bool) -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8];

        let mut quantization = vec![0];
        quantization.extend([1; 64]);
        append_segment(&mut jpeg, 0xdb, &quantization);
        let [width_high, width_low] = width.to_be_bytes();
        append_segment(
            &mut jpeg,
            0xc2,
            &[8, 0, 8, width_high, width_low, 1, 1, 0x11, 0],
        );

        let mut huffman = vec![0x00, 1];
        huffman.extend([0; 15]);
        huffman.push(1);
        huffman.extend([0x10, 1, 1]);
        huffman.extend([0; 14]);
        huffman.extend([0x01, 0x00]);
        append_segment(&mut jpeg, 0xc4, &huffman);
        if restart {
            append_segment(&mut jpeg, 0xdd, &[0, 1]);
        }

        let repeat = |single: &[u8]| {
            if restart {
                let mut entropy = single.to_vec();
                entropy.extend([0xff, 0xd0]);
                entropy.extend_from_slice(single);
                entropy
            } else {
                single.to_vec()
            }
        };
        append_scan(&mut jpeg, 0, 0, 0, 1, &repeat(&[0x7f]));
        append_scan(&mut jpeg, 0, 0, 1, 0, &repeat(&[0xff, 0x00]));
        append_scan(&mut jpeg, 1, 63, 0, 1, &repeat(&[0x6f]));
        append_scan(&mut jpeg, 1, 63, 1, 0, &repeat(&[0xbf]));
        jpeg.extend([0xff, 0xd9]);
        jpeg
    }

    fn append_scan(
        jpeg: &mut Vec<u8>,
        spectral_start: u8,
        spectral_end: u8,
        approximation_high: u8,
        approximation_low: u8,
        entropy: &[u8],
    ) {
        append_segment(
            jpeg,
            0xda,
            &[
                1,
                1,
                0,
                spectral_start,
                spectral_end,
                (approximation_high << 4) | approximation_low,
            ],
        );
        jpeg.extend_from_slice(entropy);
    }

    fn append_segment(jpeg: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        jpeg.extend([0xff, marker]);
        jpeg.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        jpeg.extend_from_slice(payload);
    }
}
