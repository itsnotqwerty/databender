use std::{fs, io::Write, ops::Range, path::Path};

use crate::{
    filters::{bytes, FilterDomain},
    DatabenderError, FilterSpec, MediaFormat, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<std::path::PathBuf> {
    let mut encoded = fs::read(prepared.input()).map_err(|source| DatabenderError::Io {
        path: prepared.input().to_path_buf(),
        source,
    })?;
    let layout = parse(&encoded)?;
    let original = encoded.clone();

    for stage in &prepared.plan().stages {
        match stage.domain {
            FilterDomain::EncodedPayload => bytes::apply(
                &stage.filters,
                &mut encoded[layout.data.clone()],
                stage.seed,
            )?,
            FilterDomain::PcmAudio => apply_pcm_noise(
                &stage.filters,
                &mut encoded[layout.data.clone()],
                layout.format,
                stage.seed,
            )?,
            _ => unreachable!("WAV capability validation excludes other domains"),
        }
    }

    let expected_layout = layout.clone();
    prepared.publish_with(
        move |candidate| candidate.write_all(&encoded),
        move |candidate| validate(candidate, &original, &expected_layout),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmFormat {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub block_align: u16,
}

impl PcmFormat {
    pub fn bytes_per_sample(self) -> usize {
        usize::from(self.bits_per_sample / 8)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PcmLayout {
    pub format: PcmFormat,
    pub data: Range<usize>,
}

impl PcmLayout {
    pub fn frame_count(&self) -> usize {
        self.data.len() / usize::from(self.format.block_align)
    }

    pub fn sample_count(&self) -> usize {
        self.data.len() / self.format.bytes_per_sample()
    }
}

pub fn parse(input: &[u8]) -> Result<PcmLayout> {
    if input.len() < 12 || &input[..4] != b"RIFF" || &input[8..12] != b"WAVE" {
        return Err(invalid_wav("missing RIFF/WAVE header"));
    }
    let riff_size = read_u32(&input[4..8]) as usize;
    let riff_end = riff_size
        .checked_add(8)
        .filter(|end| *end == input.len())
        .ok_or_else(|| invalid_wav("RIFF size does not match file length"))?;

    let mut format = None;
    let mut data = None;
    let mut offset = 12;
    while offset < riff_end {
        if riff_end - offset < 8 {
            return Err(invalid_wav("truncated chunk header"));
        }
        let kind = &input[offset..offset + 4];
        let size = read_u32(&input[offset + 4..offset + 8]) as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start
            .checked_add(size)
            .filter(|end| *end <= riff_end)
            .ok_or_else(|| invalid_wav("chunk size exceeds RIFF bounds"))?;

        match kind {
            b"fmt " if format.is_none() => {
                format = Some(parse_format(&input[payload_start..payload_end])?);
            }
            b"fmt " => return Err(invalid_wav("multiple fmt chunks are not supported")),
            b"data" if data.is_none() => data = Some(payload_start..payload_end),
            b"data" => return Err(invalid_wav("multiple data chunks are not supported")),
            _ => {}
        }

        offset = payload_end
            .checked_add(size & 1)
            .filter(|end| *end <= riff_end)
            .ok_or_else(|| invalid_wav("missing odd-sized chunk padding"))?;
    }

    let format = format.ok_or_else(|| invalid_wav("missing fmt chunk"))?;
    let data = data.ok_or_else(|| invalid_wav("missing data chunk"))?;
    if data.len() % usize::from(format.block_align) != 0 {
        return Err(invalid_wav(
            "data chunk does not contain whole sample frames",
        ));
    }

    Ok(PcmLayout { format, data })
}

fn apply_pcm_noise(
    filters: &[FilterSpec],
    samples: &mut [u8],
    format: PcmFormat,
    seed: u64,
) -> Result<()> {
    for (filter_index, filter) in filters.iter().enumerate() {
        let FilterSpec::AudioNoise {
            probability,
            amplitude,
        } = filter
        else {
            return Err(invalid_wav(format!(
                "filter {} is not a PCM-audio filter",
                filter.name()
            )));
        };
        let mut random = Random::new(seed.wrapping_add(filter_index as u64));
        for sample in samples.chunks_exact_mut(format.bytes_per_sample()) {
            if random.fraction() >= *probability {
                continue;
            }
            let (value, minimum, maximum) = decode_sample(sample, format.bits_per_sample);
            let delta =
                ((random.fraction() * 2.0 - 1.0) * *amplitude * maximum as f64).round() as i64;
            encode_sample(sample, (value + delta).clamp(minimum, maximum));
        }
    }
    Ok(())
}

pub(crate) fn apply_pcm_stage(
    encoded: &mut [u8],
    filters: &[FilterSpec],
    seed: u64,
) -> Result<PcmFormat> {
    let layout = parse(encoded)?;
    apply_pcm_noise(filters, &mut encoded[layout.data], layout.format, seed)?;
    Ok(layout.format)
}

fn decode_sample(sample: &[u8], bits: u16) -> (i64, i64, i64) {
    match bits {
        8 => (i64::from(sample[0]) - 128, -128, 127),
        16 => (
            i64::from(i16::from_le_bytes([sample[0], sample[1]])),
            i64::from(i16::MIN),
            i64::from(i16::MAX),
        ),
        24 => {
            let raw = i32::from_le_bytes([
                sample[0],
                sample[1],
                sample[2],
                if sample[2] & 0x80 == 0 { 0 } else { 0xff },
            ]);
            (i64::from(raw), -8_388_608, 8_388_607)
        }
        32 => (
            i64::from(i32::from_le_bytes([
                sample[0], sample[1], sample[2], sample[3],
            ])),
            i64::from(i32::MIN),
            i64::from(i32::MAX),
        ),
        _ => unreachable!("format parser validates PCM bit depth"),
    }
}

fn encode_sample(sample: &mut [u8], value: i64) {
    match sample.len() {
        1 => sample[0] = (value + 128) as u8,
        2 => sample.copy_from_slice(&(value as i16).to_le_bytes()),
        3 => sample.copy_from_slice(&(value as i32).to_le_bytes()[..3]),
        4 => sample.copy_from_slice(&(value as i32).to_le_bytes()),
        _ => unreachable!("format parser validates PCM sample width"),
    }
}

fn validate(path: &Path, original: &[u8], expected: &PcmLayout) -> Result<()> {
    if MediaFormat::detect(path)? != MediaFormat::Wav {
        return Err(invalid_wav("candidate format changed"));
    }
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let actual = parse(&encoded)?;
    if actual != *expected {
        return Err(invalid_wav("PCM format or data layout changed"));
    }
    if encoded[..actual.data.start] != original[..expected.data.start]
        || encoded[actual.data.end..] != original[expected.data.end..]
    {
        return Err(invalid_wav("bytes outside the data chunk changed"));
    }
    Ok(())
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

    fn fraction(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1_u64 << 53) as f64);
        ((self.next() >> 11) as f64) * SCALE
    }
}

fn parse_format(input: &[u8]) -> Result<PcmFormat> {
    if input.len() < 16 {
        return Err(invalid_wav("fmt chunk is shorter than 16 bytes"));
    }
    if read_u16(&input[..2]) != 1 {
        return Err(invalid_wav("only integer PCM format 1 is supported"));
    }
    let channels = read_u16(&input[2..4]);
    let sample_rate = read_u32(&input[4..8]);
    let byte_rate = read_u32(&input[8..12]);
    let block_align = read_u16(&input[12..14]);
    let bits_per_sample = read_u16(&input[14..16]);
    if channels == 0 || sample_rate == 0 {
        return Err(invalid_wav("channel count and sample rate must be nonzero"));
    }
    if !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(invalid_wav("PCM samples must be 8, 16, 24, or 32 bits"));
    }

    let expected_align = channels
        .checked_mul(bits_per_sample / 8)
        .ok_or_else(|| invalid_wav("block alignment overflow"))?;
    if block_align != expected_align {
        return Err(invalid_wav("block alignment does not match PCM geometry"));
    }
    let expected_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| invalid_wav("byte rate overflow"))?;
    if byte_rate != expected_rate {
        return Err(invalid_wav("byte rate does not match PCM geometry"));
    }

    Ok(PcmFormat {
        channels,
        sample_rate,
        bits_per_sample,
        block_align,
    })
}

fn read_u16(input: &[u8]) -> u16 {
    u16::from_le_bytes([input[0], input[1]])
}

fn read_u32(input: &[u8]) -> u32 {
    u32::from_le_bytes([input[0], input[1], input[2], input[3]])
}

fn invalid_wav(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid WAV: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(data: &[u8], extra_chunk: bool) -> Vec<u8> {
        let mut chunks = Vec::new();
        chunks.extend_from_slice(b"fmt ");
        chunks.extend_from_slice(&16_u32.to_le_bytes());
        chunks.extend_from_slice(&1_u16.to_le_bytes());
        chunks.extend_from_slice(&2_u16.to_le_bytes());
        chunks.extend_from_slice(&48_000_u32.to_le_bytes());
        chunks.extend_from_slice(&192_000_u32.to_le_bytes());
        chunks.extend_from_slice(&4_u16.to_le_bytes());
        chunks.extend_from_slice(&16_u16.to_le_bytes());
        if extra_chunk {
            chunks.extend_from_slice(b"JUNK");
            chunks.extend_from_slice(&3_u32.to_le_bytes());
            chunks.extend_from_slice(&[1, 2, 3, 0]);
        }
        chunks.extend_from_slice(b"data");
        chunks.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunks.extend_from_slice(data);

        let mut output = b"RIFF".to_vec();
        output.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
        output.extend_from_slice(b"WAVE");
        output.extend_from_slice(&chunks);
        output
    }

    #[test]
    fn parses_aligned_pcm_and_skips_padded_chunks() {
        let input = fixture(&[0, 1, 2, 3, 4, 5, 6, 7], true);

        let layout = parse(&input).unwrap();

        assert_eq!(layout.format.channels, 2);
        assert_eq!(layout.format.sample_rate, 48_000);
        assert_eq!(layout.frame_count(), 2);
        assert_eq!(layout.sample_count(), 4);
        assert_eq!(&input[layout.data], &[0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn rejects_misaligned_or_inconsistent_pcm() {
        assert!(parse(&fixture(&[0, 1, 2], false)).is_err());

        let mut bad_rate = fixture(&[0, 1, 2, 3], false);
        bad_rate[28..32].copy_from_slice(&1_u32.to_le_bytes());
        assert!(parse(&bad_rate).is_err());
    }

    #[test]
    fn pcm_noise_is_deterministic_bounded_and_keeps_frames_aligned() {
        let format = PcmFormat {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            block_align: 2,
        };
        let original = [i16::MIN, -1, 0, 1, i16::MAX]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut first = original.clone();
        let mut second = original.clone();
        let filters = [FilterSpec::AudioNoise {
            probability: 1.0,
            amplitude: 1.0,
        }];

        apply_pcm_noise(&filters, &mut first, format, 42).unwrap();
        apply_pcm_noise(&filters, &mut second, format, 42).unwrap();

        assert_eq!(first, second);
        assert_ne!(first, original);
        assert_eq!(first.len() % usize::from(format.block_align), 0);
    }
}
