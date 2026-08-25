use std::ops::Range;

use crate::{DatabenderError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MpegVersion {
    Version1,
    Version2,
    Version25,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mp3Frame {
    pub frame: Range<usize>,
    pub header: Range<usize>,
    pub crc: Option<Range<usize>>,
    pub side_information: Range<usize>,
    pub main_data: Range<usize>,
    pub main_data_begin: usize,
    pub version: MpegVersion,
    pub sample_rate: u32,
    pub bitrate_kbps: u16,
    pub channels: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mp3Structure {
    pub leading_metadata: Option<Range<usize>>,
    pub trailing_metadata: Option<Range<usize>>,
    pub frames: Vec<Mp3Frame>,
}

pub fn parse(encoded: &[u8]) -> Result<Mp3Structure> {
    let leading_metadata = id3v2_range(encoded)?;
    let trailing_metadata = encoded
        .len()
        .checked_sub(128)
        .filter(|offset| encoded[*offset..].starts_with(b"TAG"))
        .map(|offset| offset..encoded.len());
    let end = trailing_metadata
        .as_ref()
        .map_or(encoded.len(), |range| range.start);
    let mut offset = leading_metadata.as_ref().map_or(0, |range| range.end);
    let mut available_reservoir = 0_usize;
    let mut frames = Vec::new();

    while offset < end {
        let frame = parse_frame(encoded, offset, end)?;
        if frame.main_data_begin > available_reservoir {
            return Err(invalid_mp3(format!(
                "frame at byte {offset} references {} reservoir bytes but only {available_reservoir} precede it",
                frame.main_data_begin
            )));
        }
        available_reservoir = available_reservoir
            .saturating_add(frame.main_data.len())
            .min(511);
        offset = frame.frame.end;
        frames.push(frame);
    }
    if frames.is_empty() {
        return Err(invalid_mp3("no MPEG Layer III frames were found"));
    }
    Ok(Mp3Structure {
        leading_metadata,
        trailing_metadata,
        frames,
    })
}

fn id3v2_range(encoded: &[u8]) -> Result<Option<Range<usize>>> {
    if !encoded.starts_with(b"ID3") {
        return Ok(None);
    }
    let header = encoded
        .get(..10)
        .ok_or_else(|| invalid_mp3("truncated ID3v2 header"))?;
    if header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return Err(invalid_mp3("ID3v2 size is not synchsafe"));
    }
    let size = header[6..10]
        .iter()
        .fold(0_usize, |size, byte| (size << 7) | usize::from(*byte));
    let footer = usize::from(header[5] & 0x10 != 0) * 10;
    let end = 10_usize
        .checked_add(size)
        .and_then(|end| end.checked_add(footer))
        .filter(|end| *end <= encoded.len())
        .ok_or_else(|| invalid_mp3("ID3v2 tag exceeds the input"))?;
    Ok(Some(0..end))
}

fn parse_frame(encoded: &[u8], offset: usize, end: usize) -> Result<Mp3Frame> {
    let header = encoded
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| invalid_mp3(format!("truncated frame header at byte {offset}")))?;
    if header[0] != 0xff || header[1] & 0xe0 != 0xe0 {
        return Err(invalid_mp3(format!("invalid frame sync at byte {offset}")));
    }
    let version = match (header[1] >> 3) & 0x03 {
        0 => MpegVersion::Version25,
        2 => MpegVersion::Version2,
        3 => MpegVersion::Version1,
        _ => return Err(invalid_mp3("reserved MPEG version")),
    };
    if (header[1] >> 1) & 0x03 != 1 {
        return Err(invalid_mp3("only MPEG Layer III frames are supported"));
    }
    let bitrate_index = usize::from(header[2] >> 4);
    let sample_rate_index = usize::from((header[2] >> 2) & 0x03);
    if bitrate_index == 0 || bitrate_index == 15 || sample_rate_index == 3 {
        return Err(invalid_mp3(
            "free/reserved bitrate or sample-rate indexes are unsupported",
        ));
    }
    let bitrate_kbps = match version {
        MpegVersion::Version1 => [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ][bitrate_index],
        MpegVersion::Version2 | MpegVersion::Version25 => {
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][bitrate_index]
        }
    };
    let base_rate = [44_100, 48_000, 32_000][sample_rate_index];
    let sample_rate = match version {
        MpegVersion::Version1 => base_rate,
        MpegVersion::Version2 => base_rate / 2,
        MpegVersion::Version25 => base_rate / 4,
    };
    let coefficient = if version == MpegVersion::Version1 {
        144_000
    } else {
        72_000
    };
    let frame_length = coefficient * usize::from(bitrate_kbps) / sample_rate as usize
        + usize::from(header[2] & 0x02 != 0);
    let frame_end = offset
        .checked_add(frame_length)
        .filter(|frame_end| *frame_end <= end)
        .ok_or_else(|| invalid_mp3(format!("frame at byte {offset} exceeds the audio payload")))?;
    let channels = if header[3] >> 6 == 3 { 1 } else { 2 };
    let protected = header[1] & 0x01 == 0;
    let crc = protected.then(|| offset + 4..offset + 6);
    let side_start = offset + 4 + usize::from(protected) * 2;
    let side_length = match (version, channels) {
        (MpegVersion::Version1, 1) => 17,
        (MpegVersion::Version1, _) => 32,
        (_, 1) => 9,
        (_, _) => 17,
    };
    let side_end = side_start + side_length;
    if side_end >= frame_end {
        return Err(invalid_mp3(format!(
            "frame at byte {offset} has no main-data payload"
        )));
    }
    let side = &encoded[side_start..side_end];
    let main_data_begin = if version == MpegVersion::Version1 {
        (usize::from(side[0]) << 1) | usize::from(side[1] >> 7)
    } else {
        usize::from(side[0])
    };

    Ok(Mp3Frame {
        frame: offset..frame_end,
        header: offset..offset + 4,
        crc,
        side_information: side_start..side_end,
        main_data: side_end..frame_end,
        main_data_begin,
        version,
        sample_rate,
        bitrate_kbps,
        channels,
    })
}

fn invalid_mp3(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid MP3 structure: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(protected: bool, mono: bool, main_data_begin: usize) -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[
            0xff,
            if protected { 0xfa } else { 0xfb },
            0x90,
            if mono { 0xc0 } else { 0x00 },
        ]);
        let side = 4 + usize::from(protected) * 2;
        frame[side] = (main_data_begin >> 1) as u8;
        frame[side + 1] = (main_data_begin as u8 & 1) << 7;
        frame
    }

    #[test]
    fn locates_metadata_crc_side_information_and_main_data() {
        let mut encoded = b"ID3\x04\x00\x00\x00\x00\x00\x04meta".to_vec();
        encoded.extend(frame(true, true, 0));
        encoded.extend(*b"TAG");
        encoded.resize(encoded.len() + 125, 0);

        let structure = parse(&encoded).unwrap();

        assert_eq!(structure.leading_metadata, Some(0..14));
        assert_eq!(structure.trailing_metadata, Some(431..559));
        assert_eq!(structure.frames.len(), 1);
        assert_eq!(structure.frames[0].header, 14..18);
        assert_eq!(structure.frames[0].crc, Some(18..20));
        assert_eq!(structure.frames[0].side_information, 20..37);
        assert_eq!(structure.frames[0].main_data, 37..431);
        assert_eq!(structure.frames[0].channels, 1);
    }

    #[test]
    fn validates_main_data_reservoir_references() {
        let mut valid = frame(false, false, 0);
        valid.extend(frame(false, false, 100));
        assert_eq!(parse(&valid).unwrap().frames[1].main_data_begin, 100);

        let invalid = frame(false, false, 1);
        assert!(parse(&invalid)
            .unwrap_err()
            .to_string()
            .contains("reservoir bytes"));
    }

    #[test]
    fn rejects_reserved_and_truncated_frames() {
        let mut reserved = frame(false, false, 0);
        reserved[2] = 0xf0;
        assert!(parse(&reserved).is_err());
        assert!(parse(&frame(false, false, 0)[..100]).is_err());
    }
}
