use crate::{codecs::mp3_frames, DatabenderError, FilterSpec, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationImpact {
    pub eligible_frames: usize,
    pub eligible_bytes: usize,
    pub mutated_bytes: usize,
}

pub fn apply(filter: &FilterSpec, encoded: &mut [u8], seed: u64) -> Result<MutationImpact> {
    let FilterSpec::Mp3MainDataNoise {
        byte_budget,
        start_frame,
        frame_count,
        intensity,
    } = filter
    else {
        return Err(DatabenderError::OutputValidation {
            reason: format!("filter {} is not an MP3 main-data filter", filter.name()),
        });
    };
    let structure = mp3_frames::parse(encoded)?;
    let available_frames = structure.frames.len().saturating_sub(*start_frame);
    let selected_frames = if *frame_count == 0 {
        available_frames
    } else {
        available_frames.min(*frame_count)
    };
    if selected_frames == 0 {
        return Err(DatabenderError::InvalidParameter {
            parameter: "mp3-main-data-noise.start_frame".to_owned(),
            reason: format!(
                "selected no frames from an MP3 containing {} frames",
                structure.frames.len()
            ),
        });
    }

    let frames = structure.frames[*start_frame..*start_frame + selected_frames]
        .iter()
        .filter(|frame| frame.crc.is_none())
        .collect::<Vec<_>>();
    if frames.is_empty() {
        return Err(DatabenderError::InvalidParameter {
            parameter: "mp3-main-data-noise.frame_count".to_owned(),
            reason: "selected frames are CRC-protected and cannot be mutated safely".to_owned(),
        });
    }
    let mut eligible = frames
        .iter()
        .flat_map(|frame| frame.main_data.clone())
        .collect::<Vec<_>>();
    let eligible_bytes = eligible.len();
    let mutated_bytes = (*byte_budget).min(eligible_bytes);
    let mut random = Random::new(seed);
    for index in 0..mutated_bytes {
        let selected = index + random.index(eligible.len() - index);
        eligible.swap(index, selected);
        let byte = &mut encoded[eligible[index]];
        let bit_count = (*intensity * 8.0).ceil() as usize;
        let mut mask = 0_u8;
        while mask.count_ones() < bit_count as u32 {
            mask |= 1 << random.index(8);
        }
        *byte ^= mask;
    }

    Ok(MutationImpact {
        eligible_frames: frames.len(),
        eligible_bytes,
        mutated_bytes: if *intensity == 0.0 { 0 } else { mutated_bytes },
    })
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

    fn frame(main_data_begin: usize, fill: u8) -> Vec<u8> {
        let mut frame = vec![fill; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        frame[4] = (main_data_begin >> 1) as u8;
        frame[5] = (main_data_begin as u8 & 1) << 7;
        frame
    }

    #[test]
    fn mutates_only_targeted_main_data_with_a_deterministic_budget() {
        let mut original = b"ID3\x04\x00\x00\x00\x00\x00\x04meta".to_vec();
        original.extend(frame(0, 0x55));
        original.extend(frame(100, 0xaa));
        original.extend(*b"TAG");
        original.resize(original.len() + 125, 0);
        let filter = FilterSpec::parse(
            "mp3-main-data-noise:byte_budget=12,start_frame=1,frame_count=1,intensity=0.25",
        )
        .unwrap();
        let mut first = original.clone();
        let mut second = original.clone();

        let impact = apply(&filter, &mut first, 42).unwrap();
        apply(&filter, &mut second, 42).unwrap();
        let structure = mp3_frames::parse(&original).unwrap();

        assert_eq!(first, second);
        assert_eq!(impact.eligible_frames, 1);
        assert_eq!(impact.mutated_bytes, 12);
        assert_eq!(first.len(), original.len());
        for (index, (before, after)) in original.iter().zip(&first).enumerate() {
            if before != after {
                assert!(structure.frames[1].main_data.contains(&index));
            }
        }
        assert_eq!(
            original
                .iter()
                .zip(&first)
                .filter(|(before, after)| before != after)
                .count(),
            12
        );
    }

    #[test]
    fn rejects_frame_targets_beyond_the_stream() {
        let mut encoded = frame(0, 0x55);
        let filter = FilterSpec::parse("mp3-main-data-noise:start_frame=2").unwrap();

        assert!(apply(&filter, &mut encoded, 42).is_err());
    }

    #[test]
    fn rejects_targets_containing_only_crc_protected_frames() {
        let mut encoded = vec![0x55; 417];
        encoded[..4].copy_from_slice(&[0xff, 0xfa, 0x90, 0x00]);
        encoded[4..6].copy_from_slice(&[0x12, 0x34]);
        encoded[6] = 0;
        encoded[7] = 0;
        let filter = FilterSpec::parse("mp3-main-data-noise").unwrap();
        let original = encoded.clone();

        let error = apply(&filter, &mut encoded, 42).unwrap_err();

        assert!(error.to_string().contains("CRC-protected"));
        assert_eq!(encoded, original);
    }
}
