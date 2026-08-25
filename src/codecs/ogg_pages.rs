use std::{collections::HashMap, ops::Range};

use crate::{DatabenderError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OggCodec {
    Vorbis,
    Opus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OggPage {
    pub bytes: Range<usize>,
    pub serial: u32,
    pub sequence: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketRange {
    pub page: usize,
    pub bytes: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OggPacket {
    pub serial: u32,
    pub index: usize,
    pub codec: OggCodec,
    pub ranges: Vec<PacketRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OggStructure {
    pub pages: Vec<OggPage>,
    pub packets: Vec<OggPacket>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationImpact {
    pub eligible_packets: usize,
    pub eligible_bytes: usize,
    pub mutated_bytes: usize,
}

#[derive(Default)]
struct StreamState {
    codec: Option<OggCodec>,
    packet_index: usize,
    partial: Vec<PacketRange>,
}

pub fn parse(encoded: &[u8]) -> Result<OggStructure> {
    let mut pages = Vec::new();
    let mut packets = Vec::new();
    let mut streams = HashMap::<u32, StreamState>::new();
    let mut sequences = HashMap::<u32, u32>::new();
    let mut offset = 0;

    while offset < encoded.len() {
        if encoded.len() - offset < 27 || &encoded[offset..offset + 4] != b"OggS" {
            return Err(invalid("missing or truncated Ogg page header"));
        }
        if encoded[offset + 4] != 0 {
            return Err(invalid("unsupported Ogg bitstream version"));
        }
        let segment_count = encoded[offset + 26] as usize;
        let header_end = offset
            .checked_add(27 + segment_count)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| invalid("truncated Ogg lacing table"))?;
        let payload_len = encoded[offset + 27..header_end]
            .iter()
            .map(|length| *length as usize)
            .sum::<usize>();
        let page_end = header_end
            .checked_add(payload_len)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| invalid("truncated Ogg page payload"))?;
        if page_crc(&encoded[offset..page_end]) != read_u32(encoded, offset + 22) {
            return Err(invalid("Ogg page CRC mismatch"));
        }

        let serial = read_u32(encoded, offset + 14);
        let sequence = read_u32(encoded, offset + 18);
        if let Some(previous) = sequences.insert(serial, sequence) {
            if sequence != previous.wrapping_add(1) {
                return Err(invalid(format!(
                    "logical stream {serial} page sequence jumped from {previous} to {sequence}"
                )));
            }
        }
        let page_index = pages.len();
        pages.push(OggPage {
            bytes: offset..page_end,
            serial,
            sequence,
        });

        let state = streams.entry(serial).or_default();
        let continued = encoded[offset + 5] & 0x01 != 0;
        if continued != !state.partial.is_empty() {
            return Err(invalid("Ogg continued-packet flag does not match lacing"));
        }
        let mut payload_offset = header_end;
        for length in &encoded[offset + 27..header_end] {
            let range = payload_offset..payload_offset + *length as usize;
            state.partial.push(PacketRange {
                page: page_index,
                bytes: range,
            });
            payload_offset += *length as usize;
            if *length < 255 {
                let codec = match state.codec {
                    Some(codec) => codec,
                    None => {
                        let codec = identify_codec(encoded, &state.partial)?;
                        state.codec = Some(codec);
                        codec
                    }
                };
                packets.push(OggPacket {
                    serial,
                    index: state.packet_index,
                    codec,
                    ranges: std::mem::take(&mut state.partial),
                });
                state.packet_index += 1;
            }
        }
        offset = page_end;
    }

    if streams.values().any(|state| !state.partial.is_empty()) {
        return Err(invalid("unterminated Ogg packet"));
    }
    if pages.is_empty() {
        return Err(invalid("container contains no pages"));
    }
    Ok(OggStructure { pages, packets })
}

pub fn mutate(
    encoded: &mut [u8],
    byte_budget: usize,
    start_packet: usize,
    packet_count: usize,
    intensity: f64,
    seed: u64,
) -> Result<MutationImpact> {
    let structure = parse(encoded)?;
    let eligible = structure
        .packets
        .iter()
        .filter(|packet| packet.index >= header_packet_count(packet.codec))
        .collect::<Vec<_>>();
    let available = eligible.len().saturating_sub(start_packet);
    let selected_count = if packet_count == 0 {
        available
    } else {
        available.min(packet_count)
    };
    if selected_count == 0 {
        return Err(invalid(
            "packet target selects no Vorbis or Opus audio packets",
        ));
    }
    let selected = &eligible[start_packet..start_packet + selected_count];
    let mut bytes = selected
        .iter()
        .flat_map(|packet| &packet.ranges)
        .flat_map(|range| range.bytes.clone().map(move |offset| (offset, range.page)))
        .collect::<Vec<_>>();
    let eligible_bytes = bytes.len();
    let mutated_bytes = byte_budget.min(eligible_bytes);
    let mut random = Random::new(seed);
    let mut touched_pages = Vec::new();
    let bit_count = (intensity.clamp(0.0, 1.0) * 8.0).ceil() as usize;
    for index in 0..mutated_bytes {
        let chosen = index + random.index(bytes.len() - index);
        bytes.swap(index, chosen);
        let (offset, page) = bytes[index];
        let mut mask = 0_u8;
        while mask.count_ones() < bit_count as u32 {
            mask |= 1 << random.index(8);
        }
        encoded[offset] ^= mask;
        if bit_count > 0 && !touched_pages.contains(&page) {
            touched_pages.push(page);
        }
    }
    for page in touched_pages {
        repair_page_crc(encoded, structure.pages[page].bytes.clone());
    }

    Ok(MutationImpact {
        eligible_packets: selected_count,
        eligible_bytes,
        mutated_bytes: if bit_count == 0 { 0 } else { mutated_bytes },
    })
}

fn identify_codec(encoded: &[u8], ranges: &[PacketRange]) -> Result<OggCodec> {
    let prefix = ranges
        .iter()
        .flat_map(|range| encoded[range.bytes.clone()].iter().copied())
        .take(8)
        .collect::<Vec<_>>();
    if prefix.starts_with(b"\x01vorbis") {
        Ok(OggCodec::Vorbis)
    } else if prefix.starts_with(b"OpusHead") {
        Ok(OggCodec::Opus)
    } else {
        Err(invalid("unsupported logical stream codec"))
    }
}

fn header_packet_count(codec: OggCodec) -> usize {
    match codec {
        OggCodec::Vorbis => 3,
        OggCodec::Opus => 2,
    }
}

fn repair_page_crc(encoded: &mut [u8], range: Range<usize>) {
    encoded[range.start + 22..range.start + 26].fill(0);
    let checksum = page_crc(&encoded[range.clone()]);
    encoded[range.start + 22..range.start + 26].copy_from_slice(&checksum.to_le_bytes());
}

fn page_crc(page: &[u8]) -> u32 {
    let mut checksum = 0_u32;
    for (index, byte) in page.iter().enumerate() {
        let byte = if (22..26).contains(&index) { 0 } else { *byte };
        checksum ^= (byte as u32) << 24;
        for _ in 0..8 {
            checksum = if checksum & 0x8000_0000 != 0 {
                (checksum << 1) ^ 0x04c1_1db7
            } else {
                checksum << 1
            };
        }
    }
    checksum
}

fn read_u32(encoded: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        encoded[offset..offset + 4]
            .try_into()
            .expect("checked page header"),
    )
}

fn invalid(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid Ogg: {}", reason.into()),
    }
}

struct Random(u64);

impl Random {
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
        (self.next() % length as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(serial: u32, sequence: u32, flags: u8, lacing: &[u8], payload: &[u8]) -> Vec<u8> {
        assert_eq!(
            lacing.iter().map(|value| *value as usize).sum::<usize>(),
            payload.len()
        );
        let mut encoded = Vec::with_capacity(27 + lacing.len() + payload.len());
        encoded.extend_from_slice(b"OggS\0");
        encoded.push(flags);
        encoded.extend_from_slice(&0_u64.to_le_bytes());
        encoded.extend_from_slice(&serial.to_le_bytes());
        encoded.extend_from_slice(&sequence.to_le_bytes());
        encoded.extend_from_slice(&0_u32.to_le_bytes());
        encoded.push(lacing.len() as u8);
        encoded.extend_from_slice(lacing);
        encoded.extend_from_slice(payload);
        let checksum = page_crc(&encoded);
        encoded[22..26].copy_from_slice(&checksum.to_le_bytes());
        encoded
    }

    fn vorbis_stream() -> Vec<u8> {
        let mut encoded = page(7, 0, 0x02, &[7], b"\x01vorbis");
        encoded.extend(page(7, 1, 0, &[7], b"comment"));
        encoded.extend(page(7, 2, 0, &[5], b"setup"));
        encoded.extend(page(7, 3, 0, &[255], &[0x55; 255]));
        encoded.extend(page(7, 4, 0x01, &[10], &[0xaa; 10]));
        encoded
    }

    #[test]
    fn mutates_continued_audio_packets_and_repairs_page_crcs() {
        let original = vorbis_stream();
        let mut first = original.clone();
        let mut second = original.clone();

        let impact = mutate(&mut first, 8, 0, 0, 0.125, 42).unwrap();
        mutate(&mut second, 8, 0, 0, 0.125, 42).unwrap();
        let reparsed = parse(&first).unwrap();
        let original_structure = parse(&original).unwrap();

        assert_eq!(first, second);
        assert_eq!(impact.eligible_packets, 1);
        assert_eq!(impact.eligible_bytes, 265);
        assert_eq!(impact.mutated_bytes, 8);
        assert_eq!(reparsed.packets[3].ranges.len(), 2);
        assert_eq!(
            original_structure.packets[3]
                .ranges
                .iter()
                .flat_map(|range| range.bytes.clone())
                .filter(|offset| first[*offset] != original[*offset])
                .count(),
            8
        );
        for (offset, (left, right)) in first.iter().zip(&original).enumerate() {
            if left != right
                && !original_structure.packets[3]
                    .ranges
                    .iter()
                    .any(|range| range.bytes.contains(&offset))
            {
                assert!(original_structure.pages[3..]
                    .iter()
                    .any(|page| (page.bytes.start + 22..page.bytes.start + 26).contains(&offset)));
            }
        }
    }

    #[test]
    fn rejects_sequence_discontinuity() {
        let mut encoded = page(7, 0, 0x02, &[8], b"OpusHead");
        encoded.extend(page(7, 2, 0, &[7], b"OpusTag"));

        assert!(parse(&encoded)
            .unwrap_err()
            .to_string()
            .contains("sequence jumped"));
    }
}
