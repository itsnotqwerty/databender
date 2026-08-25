use std::ops::Range;

use crate::{DatabenderError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationImpact {
    pub eligible_bytes: usize,
    pub mutated_bytes: usize,
}

pub fn mutate_packet(
    codec: &str,
    packet: &mut [u8],
    nal_length_size: Option<usize>,
    byte_budget: usize,
    intensity: f64,
    seed: u64,
) -> Result<MutationImpact> {
    if !(0.0..=1.0).contains(&intensity) {
        return Err(invalid("intensity must be between 0 and 1"));
    }
    let ranges = match codec {
        "h264" => nal_ranges(packet, nal_length_size, NalCodec::H264)?,
        "hevc" | "h265" => nal_ranges(packet, nal_length_size, NalCodec::H265)?,
        "vp8" => vp8_ranges(packet)?,
        "vp9" => vp9_ranges(packet)?,
        "av1" => av1_ranges(packet)?,
        _ => return Err(invalid(format!("unsupported encoded video codec {codec}"))),
    };
    let eligible = ranges
        .iter()
        .flat_map(|range| range.clone())
        .collect::<Vec<_>>();
    let mutation_count = byte_budget.min(eligible.len());
    if mutation_count == 0 || intensity == 0.0 {
        return Ok(MutationImpact {
            eligible_bytes: eligible.len(),
            mutated_bytes: 0,
        });
    }

    let mut candidates = eligible;
    let mut rng = SeededRng::new(seed);
    for index in 0..mutation_count {
        let selected = index + rng.index(candidates.len() - index);
        candidates.swap(index, selected);
        let mask_bits = ((intensity * 8.0).ceil() as u8).clamp(1, 8);
        let mask = (rng.next() as u8) & ((1_u16 << mask_bits) - 1) as u8;
        packet[candidates[index]] ^= mask.max(1);
    }
    Ok(MutationImpact {
        eligible_bytes: candidates.len(),
        mutated_bytes: mutation_count,
    })
}

#[derive(Clone, Copy)]
enum NalCodec {
    H264,
    H265,
}

fn nal_ranges(
    packet: &[u8],
    nal_length_size: Option<usize>,
    codec: NalCodec,
) -> Result<Vec<Range<usize>>> {
    let length_size = nal_length_size
        .filter(|size| (1..=4).contains(size))
        .ok_or_else(|| invalid("NAL length size must be between 1 and 4"))?;
    let mut offset = 0;
    let mut ranges = Vec::new();
    while offset < packet.len() {
        let length_end = offset
            .checked_add(length_size)
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| invalid("truncated NAL length"))?;
        let length = packet[offset..length_end]
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        let start = length_end;
        let end = start
            .checked_add(length)
            .filter(|end| length > 0 && *end <= packet.len())
            .ok_or_else(|| invalid("NAL unit exceeds packet bounds"))?;
        let (vcl, header_size) = match codec {
            NalCodec::H264 => (matches!(packet[start] & 0x1f, 1 | 5), 1),
            NalCodec::H265 => {
                if length < 2 {
                    return Err(invalid("H.265 NAL unit is shorter than its header"));
                }
                (((packet[start] >> 1) & 0x3f) <= 31, 2)
            }
        };
        if vcl {
            let payload = (start + header_size + 16).min(end);
            if payload < end {
                ranges.push(payload..end);
            }
        }
        offset = end;
    }
    Ok(ranges)
}

fn vp8_ranges(packet: &[u8]) -> Result<Vec<Range<usize>>> {
    if packet.len() < 3 {
        return Err(invalid("VP8 frame is shorter than its frame tag"));
    }
    let keyframe = packet[0] & 1 == 0;
    let header = if keyframe {
        if packet.len() < 10 || packet[3..6] != [0x9d, 0x01, 0x2a] {
            return Err(invalid("VP8 keyframe has an invalid start code"));
        }
        10
    } else {
        3
    };
    Ok(payload_tail(packet.len(), header + 16))
}

fn vp9_ranges(packet: &[u8]) -> Result<Vec<Range<usize>>> {
    if packet.is_empty() || packet[0] >> 6 != 0b10 {
        return Err(invalid("VP9 frame has an invalid frame marker"));
    }
    Ok(payload_tail(packet.len(), 16))
}

fn av1_ranges(packet: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut offset = 0;
    let mut ranges = Vec::new();
    while offset < packet.len() {
        let header = packet[offset];
        if header & 0x80 != 0 || header & 0x01 != 0 {
            return Err(invalid("AV1 OBU has invalid reserved bits"));
        }
        let obu_type = (header >> 3) & 0x0f;
        let extension = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        if !has_size {
            return Err(invalid("AV1 OBU must carry an explicit size"));
        }
        let mut cursor = offset + 1 + usize::from(extension);
        if cursor > packet.len() {
            return Err(invalid("truncated AV1 OBU extension"));
        }
        let (size, size_bytes) = read_leb128(&packet[cursor..])?;
        cursor += size_bytes;
        let end = cursor
            .checked_add(size)
            .filter(|end| *end <= packet.len())
            .ok_or_else(|| invalid("AV1 OBU exceeds packet bounds"))?;
        if obu_type == 4 && cursor < end {
            ranges.push(cursor..end);
        }
        offset = end;
    }
    Ok(ranges)
}

fn read_leb128(encoded: &[u8]) -> Result<(usize, usize)> {
    let mut value = 0_usize;
    for (index, byte) in encoded.iter().copied().take(8).enumerate() {
        value |= usize::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(invalid("invalid AV1 OBU size"))
}

fn payload_tail(length: usize, protected: usize) -> Vec<Range<usize>> {
    (protected < length)
        .then_some(protected..length)
        .into_iter()
        .collect()
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

fn invalid(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid encoded video packet: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn length_prefixed(units: &[&[u8]]) -> Vec<u8> {
        let mut packet = Vec::new();
        for unit in units {
            packet.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            packet.extend_from_slice(unit);
        }
        packet
    }

    #[test]
    fn mutates_h264_vcl_without_touching_parameter_sets_or_headers() {
        let sps = [0x67; 24];
        let pps = [0x68; 20];
        let slice = [0x65; 40];
        let mut packet = length_prefixed(&[&sps, &pps, &slice]);
        let original = packet.clone();

        let impact = mutate_packet("h264", &mut packet, Some(4), 8, 0.5, 42).unwrap();

        assert_eq!(impact.mutated_bytes, 8);
        assert_eq!(
            &packet[..4 + sps.len() + 4 + pps.len() + 4 + 17],
            &original[..4 + sps.len() + 4 + pps.len() + 4 + 17]
        );
        assert_ne!(packet, original);
    }

    #[test]
    fn supports_h265_vp8_vp9_and_av1_payload_classes() {
        let mut h265_nal = [0_u8; 32];
        h265_nal[..2].copy_from_slice(&[0x26, 0x01]);
        let mut h265 = length_prefixed(&[&h265_nal]);
        assert!(
            mutate_packet("hevc", &mut h265, Some(4), 2, 1.0, 1)
                .unwrap()
                .mutated_bytes
                > 0
        );

        let mut vp8 = vec![0x01; 40];
        let mut vp9 = vec![0x80; 40];
        assert_eq!(
            mutate_packet("vp8", &mut vp8, None, 2, 1.0, 2)
                .unwrap()
                .mutated_bytes,
            2
        );
        assert_eq!(
            mutate_packet("vp9", &mut vp9, None, 2, 1.0, 3)
                .unwrap()
                .mutated_bytes,
            2
        );

        let mut av1 = [vec![0x0a, 2, 1, 2], vec![0x22, 4, 3, 4, 5, 6]].concat();
        let original = av1.clone();
        assert_eq!(
            mutate_packet("av1", &mut av1, None, 2, 1.0, 4)
                .unwrap()
                .mutated_bytes,
            2
        );
        assert_eq!(&av1[..4], &original[..4]);
    }

    #[test]
    fn rejects_malformed_packet_framing() {
        assert!(mutate_packet("h264", &mut [0, 0, 0, 8, 0x65], Some(4), 1, 1.0, 1).is_err());
        assert!(mutate_packet("vp8", &mut [0, 0], None, 1, 1.0, 1).is_err());
        assert!(mutate_packet("vp9", &mut [0], None, 1, 1.0, 1).is_err());
        assert!(mutate_packet("av1", &mut [0x08], None, 1, 1.0, 1).is_err());
    }

    #[test]
    fn seeded_codec_outputs_are_stable() {
        let mut h264 = length_prefixed(&[&[0x65; 24]]);
        let mut h265 = length_prefixed(&[&[
            0x26, 0x01, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
            23,
        ]]);
        let mut vp8 = [vec![0x01; 19], vec![2, 3, 4, 5, 6, 7, 8, 9]].concat();
        let mut vp9 = [vec![0x80; 16], vec![1, 2, 3, 4, 5, 6, 7, 8]].concat();
        let mut av1 = vec![0x22, 8, 1, 2, 3, 4, 5, 6, 7, 8];
        for (codec, packet, length_size) in [
            ("h264", &mut h264, Some(4)),
            ("hevc", &mut h265, Some(4)),
            ("vp8", &mut vp8, None),
            ("vp9", &mut vp9, None),
            ("av1", &mut av1, None),
        ] {
            mutate_packet(codec, packet, length_size, 4, 0.5, 42).unwrap();
        }
        assert_eq!(
            h264,
            [
                0, 0, 0, 24, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65,
                0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x65, 0x61, 0x63, 0x65, 0x61, 0x66, 0x65,
            ]
        );
        assert_eq!(
            h265,
            [
                0, 0, 0, 24, 0x26, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 20,
                16, 16, 21, 18, 23,
            ]
        );
        assert_eq!(
            vp8,
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 7, 4, 1, 6, 4, 14, 9,]
        );
        assert_eq!(
            vp9,
            [
                0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
                0x80, 0x80, 1, 6, 3, 0, 5, 5, 1, 8,
            ]
        );
        assert_eq!(av1, [0x22, 8, 1, 6, 3, 0, 5, 5, 1, 8]);
    }

    proptest! {
        #[test]
        fn vp9_mutation_is_deterministic_bounded_and_length_preserving(
            tail in proptest::collection::vec(any::<u8>(), 16..512),
            budget in 0_usize..64,
            seed in any::<u64>(),
        ) {
            let mut packet = vec![0x80; 16];
            packet.extend_from_slice(&tail);
            let original = packet.clone();
            let mut replay = packet.clone();

            let impact = mutate_packet("vp9", &mut packet, None, budget, 0.5, seed).unwrap();
            let replay_impact = mutate_packet("vp9", &mut replay, None, budget, 0.5, seed).unwrap();

            prop_assert_eq!(&packet, &replay);
            prop_assert_eq!(impact, replay_impact);
            prop_assert_eq!(packet.len(), original.len());
            prop_assert_eq!(&packet[..16], &original[..16]);
            let changed = packet.iter().zip(&original).filter(|(left, right)| left != right).count();
            prop_assert_eq!(changed, impact.mutated_bytes);
            prop_assert!(impact.mutated_bytes <= budget);
            prop_assert!(impact.mutated_bytes <= impact.eligible_bytes);
        }

        #[test]
        fn arbitrary_packets_never_panic_or_change_length_on_success(
            codec_index in 0_usize..5,
            mut packet in proptest::collection::vec(any::<u8>(), 0..1024),
            length_size in 0_usize..6,
            budget in 0_usize..64,
            intensity in 0.0_f64..=1.0,
            seed in any::<u64>(),
        ) {
            let codecs = ["h264", "hevc", "vp8", "vp9", "av1"];
            let original_len = packet.len();
            let result = mutate_packet(
                codecs[codec_index],
                &mut packet,
                Some(length_size),
                budget,
                intensity,
                seed,
            );

            prop_assert_eq!(packet.len(), original_len);
            if let Ok(impact) = result {
                prop_assert!(impact.mutated_bytes <= budget);
                prop_assert!(impact.mutated_bytes <= impact.eligible_bytes);
            }
        }
    }
}
