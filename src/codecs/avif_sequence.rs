use crate::{DatabenderError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AnimationInfo {
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub loop_count: u32,
    pub frame_durations: Vec<u32>,
    pub has_alpha: bool,
}

impl AnimationInfo {
    pub(crate) fn equivalent_to(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.loop_count == other.loop_count
            && self.has_alpha == other.has_alpha
            && self.frame_durations.len() == other.frame_durations.len()
            && self
                .frame_durations
                .iter()
                .zip(&other.frame_durations)
                .all(|(left, right)| {
                    u64::from(*left) * u64::from(other.timescale)
                        == u64::from(*right) * u64::from(self.timescale)
                })
    }
}

pub(crate) fn parse(encoded: &[u8]) -> Result<AnimationInfo> {
    let layout = layout(encoded)?;
    let frame_durations = read_stts(encoded, layout.stts)?;
    if frame_durations.len() < 2 {
        return Err(invalid_avif(
            "sequence track contains fewer than two frames",
        ));
    }
    let media_duration = frame_durations
        .iter()
        .try_fold(0_u64, |total, duration| {
            total.checked_add(u64::from(*duration))
        })
        .ok_or_else(|| invalid_avif("frame durations overflow"))?;
    if media_duration != layout.media_duration {
        return Err(invalid_avif("sample timing does not match media duration"));
    }
    let one_play = scale_duration(
        media_duration,
        layout.media_timescale,
        layout.movie_timescale,
    )?;
    let loop_count = if layout.track_duration == u64::MAX >> 1 {
        0
    } else {
        let loops = layout
            .track_duration
            .checked_div(one_play)
            .filter(|loops| {
                *loops > 0 && one_play.checked_mul(*loops) == Some(layout.track_duration)
            })
            .ok_or_else(|| invalid_avif("track duration is not an integer loop count"))?;
        u32::try_from(loops).map_err(|_| invalid_avif("loop count exceeds u32"))?
    };
    Ok(AnimationInfo {
        width: layout.width,
        height: layout.height,
        timescale: layout.media_timescale,
        loop_count,
        frame_durations,
        has_alpha: layout.has_alpha,
    })
}

pub(crate) fn repair_timing(encoded: &mut [u8], expected: &AnimationInfo) -> Result<()> {
    let layout = layout(encoded)?;
    let actual = read_stts(encoded, layout.stts)?;
    if actual.len() != expected.frame_durations.len() {
        return Err(invalid_avif(
            "encoded frame count changed during timing repair",
        ));
    }
    let scaled = expected
        .frame_durations
        .iter()
        .map(|duration| {
            scale_duration(
                u64::from(*duration),
                expected.timescale,
                layout.media_timescale,
            )
            .and_then(|duration| {
                u32::try_from(duration).map_err(|_| invalid_avif("frame duration exceeds u32"))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if actual[..actual.len() - 1] != scaled[..scaled.len() - 1] {
        return Err(invalid_avif(
            "encoded frame timestamps changed before the final sample",
        ));
    }
    let (last_count, last_delta) = last_stts_entry(encoded, layout.stts)?;
    if last_count != 1 && actual.last() != scaled.last() {
        return Err(invalid_avif(
            "final stts entry is shared by multiple samples",
        ));
    }
    write_u32(encoded, last_delta, *scaled.last().unwrap())?;
    let media_duration = scaled
        .iter()
        .try_fold(0_u64, |total, duration| {
            total.checked_add(u64::from(*duration))
        })
        .ok_or_else(|| invalid_avif("media duration overflows"))?;
    write_duration(encoded, layout.mdhd_duration, media_duration)?;
    let one_play = scale_duration(
        media_duration,
        layout.media_timescale,
        layout.movie_timescale,
    )?;
    write_duration(encoded, layout.elst_duration, one_play)?;
    let total = if expected.loop_count == 0 {
        u64::MAX >> 1
    } else {
        one_play
            .checked_mul(u64::from(expected.loop_count))
            .ok_or_else(|| invalid_avif("loop duration overflows"))?
    };
    write_duration(encoded, layout.tkhd_duration, total)?;
    write_duration(encoded, layout.mvhd_duration, total)?;
    let actual = parse(encoded)?;
    if !actual.equivalent_to(expected) {
        return Err(invalid_avif(format!(
            "repaired timing does not match the source sequence: expected {expected:?}, actual {actual:?}"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct BoxRange {
    data: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Layout {
    movie_timescale: u32,
    track_duration: u64,
    media_timescale: u32,
    media_duration: u64,
    width: u32,
    height: u32,
    stts: BoxRange,
    mvhd_duration: IntegerField,
    tkhd_duration: IntegerField,
    mdhd_duration: IntegerField,
    elst_duration: IntegerField,
    has_alpha: bool,
    track_id: u32,
}

#[derive(Clone, Copy)]
struct IntegerField {
    offset: usize,
    width: usize,
}

fn layout(encoded: &[u8]) -> Result<Layout> {
    let moov = find_box(encoded, 0, encoded.len(), *b"moov")?
        .ok_or_else(|| invalid_avif("missing moov box"))?;
    let mvhd = required_child(encoded, moov, *b"mvhd")?;
    let (movie_timescale, _, mvhd_duration) = media_header(encoded, mvhd, "mvhd")?;
    let mut selected: Option<Layout> = None;
    for trak in boxes(encoded, moov.data, moov.end)? {
        if box_kind(encoded, trak) != *b"trak" {
            continue;
        }
        let mdia = required_child(encoded, trak, *b"mdia")?;
        let mdhd = required_child(encoded, mdia, *b"mdhd")?;
        let minf = required_child(encoded, mdia, *b"minf")?;
        let stbl = required_child(encoded, minf, *b"stbl")?;
        let stts = required_child(encoded, stbl, *b"stts")?;
        let durations = read_stts(encoded, stts)?;
        if durations.len() < 2 {
            continue;
        }
        let tkhd = required_child(encoded, trak, *b"tkhd")?;
        let (track_id, track_duration, width, height, tkhd_duration) = track_header(encoded, tkhd)?;
        let (media_timescale, media_duration, mdhd_duration) = media_header(encoded, mdhd, "mdhd")?;
        let edts = required_child(encoded, trak, *b"edts")?;
        let elst = required_child(encoded, edts, *b"elst")?;
        let elst_duration = edit_list_duration(encoded, elst)?;
        let candidate = Layout {
            movie_timescale,
            track_duration,
            media_timescale,
            media_duration,
            width,
            height,
            stts,
            mvhd_duration,
            tkhd_duration,
            mdhd_duration,
            elst_duration,
            has_alpha: false,
            track_id,
        };
        if let Some(layout) = &mut selected {
            if layout.width != candidate.width
                || layout.height != candidate.height
                || layout.media_timescale != candidate.media_timescale
                || read_stts(encoded, layout.stts)? != durations
            {
                return Err(invalid_avif(
                    "auxiliary sequence track does not match color timing and dimensions",
                ));
            }
            if layout.has_alpha {
                return Err(invalid_avif(
                    "more than two animated tracks are unsupported",
                ));
            }
            let tref = required_child(encoded, trak, *b"tref")?;
            let auxl = required_child(encoded, tref, *b"auxl")?;
            if read_u32(encoded, auxl.data)? != layout.track_id {
                return Err(invalid_avif(
                    "auxiliary sequence track does not reference the color track",
                ));
            }
            layout.has_alpha = true;
        } else {
            selected = Some(candidate);
        }
    }
    selected.ok_or_else(|| invalid_avif("missing animated image sequence track"))
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
            data: offset + header,
            end: box_end,
        });
        offset = box_end;
    }
    Ok(result)
}

fn find_box(encoded: &[u8], start: usize, end: usize, kind: [u8; 4]) -> Result<Option<BoxRange>> {
    Ok(boxes(encoded, start, end)?
        .into_iter()
        .find(|range| box_kind(encoded, *range) == kind))
}

fn required_child(encoded: &[u8], parent: BoxRange, kind: [u8; 4]) -> Result<BoxRange> {
    find_box(encoded, parent.data, parent.end, kind)?
        .ok_or_else(|| invalid_avif(format!("missing {} box", String::from_utf8_lossy(&kind))))
}

fn box_kind(encoded: &[u8], range: BoxRange) -> [u8; 4] {
    encoded[range.data - 4..range.data].try_into().unwrap()
}

fn media_header(encoded: &[u8], range: BoxRange, name: &str) -> Result<(u32, u64, IntegerField)> {
    let version = *encoded
        .get(range.data)
        .ok_or_else(|| invalid_avif(format!("truncated {name} box")))?;
    let (timescale_offset, duration_offset) = match version {
        0 => (range.data + 12, range.data + 16),
        1 => (range.data + 20, range.data + 24),
        _ => return Err(invalid_avif(format!("unsupported {name} version"))),
    };
    let timescale = read_u32(encoded, timescale_offset)?;
    if timescale == 0 {
        return Err(invalid_avif(format!("{name} timescale is zero")));
    }
    let duration = if version == 0 {
        u64::from(read_u32(encoded, duration_offset)?)
    } else {
        read_u64(encoded, duration_offset)?
    };
    Ok((
        timescale,
        duration,
        IntegerField {
            offset: duration_offset,
            width: if version == 0 { 4 } else { 8 },
        },
    ))
}

fn track_header(encoded: &[u8], range: BoxRange) -> Result<(u32, u64, u32, u32, IntegerField)> {
    let version = *encoded
        .get(range.data)
        .ok_or_else(|| invalid_avif("truncated tkhd box"))?;
    let duration_field = match version {
        0 => IntegerField {
            offset: range.data + 20,
            width: 4,
        },
        1 => IntegerField {
            offset: range.data + 28,
            width: 8,
        },
        _ => return Err(invalid_avif("unsupported tkhd version")),
    };
    let track_id = read_u32(
        encoded,
        if version == 0 {
            range.data + 12
        } else {
            range.data + 20
        },
    )?;
    let duration = read_duration(encoded, duration_field)?;
    if range.end < range.data + 8 {
        return Err(invalid_avif("truncated tkhd dimensions"));
    }
    let width = read_u32(encoded, range.end - 8)? >> 16;
    let height = read_u32(encoded, range.end - 4)? >> 16;
    if width == 0 || height == 0 {
        return Err(invalid_avif("sequence dimensions are zero"));
    }
    Ok((track_id, duration, width, height, duration_field))
}

fn edit_list_duration(encoded: &[u8], range: BoxRange) -> Result<IntegerField> {
    let version = *encoded
        .get(range.data)
        .ok_or_else(|| invalid_avif("truncated elst box"))?;
    if read_u32(encoded, range.data + 4)? != 1 {
        return Err(invalid_avif("elst must contain one entry"));
    }
    match version {
        0 => Ok(IntegerField {
            offset: range.data + 8,
            width: 4,
        }),
        1 => Ok(IntegerField {
            offset: range.data + 8,
            width: 8,
        }),
        _ => Err(invalid_avif("unsupported elst version")),
    }
}

fn read_stts(encoded: &[u8], range: BoxRange) -> Result<Vec<u32>> {
    let entries = read_u32(encoded, range.data + 4)? as usize;
    let mut offset = range.data + 8;
    let mut durations = Vec::new();
    for _ in 0..entries {
        let count = read_u32(encoded, offset)? as usize;
        let duration = read_u32(encoded, offset + 4)?;
        if count == 0 || duration == 0 {
            return Err(invalid_avif("stts entries must be positive"));
        }
        let new_len = durations
            .len()
            .checked_add(count)
            .filter(|length| *length <= 1_000_000)
            .ok_or_else(|| invalid_avif("sequence frame count exceeds limit"))?;
        durations.resize(new_len, duration);
        offset += 8;
    }
    if offset > range.end {
        return Err(invalid_avif("stts entries exceed box bounds"));
    }
    Ok(durations)
}

fn last_stts_entry(encoded: &[u8], range: BoxRange) -> Result<(u32, usize)> {
    let entries = read_u32(encoded, range.data + 4)? as usize;
    if entries == 0 {
        return Err(invalid_avif("stts contains no entries"));
    }
    let offset = range.data + 8 + (entries - 1) * 8;
    Ok((read_u32(encoded, offset)?, offset + 4))
}

fn read_duration(encoded: &[u8], field: IntegerField) -> Result<u64> {
    match field.width {
        4 => read_u32(encoded, field.offset).map(u64::from),
        8 => read_u64(encoded, field.offset),
        _ => unreachable!(),
    }
}

fn write_duration(encoded: &mut [u8], field: IntegerField, value: u64) -> Result<()> {
    match field.width {
        4 => write_u32(
            encoded,
            field.offset,
            u32::try_from(value).map_err(|_| invalid_avif("duration exceeds version-0 field"))?,
        )?,
        8 => encoded
            .get_mut(field.offset..field.offset + 8)
            .ok_or_else(|| invalid_avif("truncated duration field"))?
            .copy_from_slice(&value.to_be_bytes()),
        _ => unreachable!(),
    }
    Ok(())
}

fn write_u32(encoded: &mut [u8], offset: usize, value: u32) -> Result<()> {
    encoded
        .get_mut(offset..offset + 4)
        .ok_or_else(|| invalid_avif("truncated integer field"))?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn scale_duration(value: u64, from: u32, to: u32) -> Result<u64> {
    let scaled = value
        .checked_mul(u64::from(to))
        .ok_or_else(|| invalid_avif("duration overflows"))?;
    if scaled % u64::from(from) != 0 {
        return Err(invalid_avif(
            "duration cannot be represented in movie timescale",
        ));
    }
    Ok(scaled / u64::from(from))
}

fn read_u32(encoded: &[u8], offset: usize) -> Result<u32> {
    encoded
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn read_u64(encoded: &[u8], offset: usize) -> Result<u64> {
    encoded
        .get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_be_bytes)
        .ok_or_else(|| invalid_avif("truncated integer field"))
}

fn invalid_avif(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid AVIF sequence: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs};

    use crate::ffmpeg::ToolRunner;
    use image::{ImageBuffer, Rgba};

    use super::*;

    #[test]
    fn parses_ffmpeg_sequence_timing_and_loop_count() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("sequence.avif");
        ToolRunner::default()
            .ffmpeg([
                OsString::from("-v"),
                OsString::from("error"),
                OsString::from("-f"),
                OsString::from("lavfi"),
                OsString::from("-i"),
                OsString::from("testsrc2=s=16x8:r=5:d=0.6"),
                OsString::from("-loop"),
                OsString::from("7"),
                output.as_os_str().to_owned(),
            ])
            .unwrap();

        assert_eq!(
            parse(&fs::read(output).unwrap()).unwrap(),
            AnimationInfo {
                width: 16,
                height: 8,
                timescale: 10240,
                loop_count: 7,
                frame_durations: vec![2048, 2048, 2048],
                has_alpha: false,
            }
        );

        let infinite = directory.path().join("infinite.avif");
        ToolRunner::default()
            .ffmpeg([
                OsString::from("-v"),
                OsString::from("error"),
                OsString::from("-f"),
                OsString::from("lavfi"),
                OsString::from("-i"),
                OsString::from("testsrc2=s=16x8:r=5:d=0.6"),
                OsString::from("-loop"),
                OsString::from("0"),
                infinite.as_os_str().to_owned(),
            ])
            .unwrap();
        assert_eq!(parse(&fs::read(infinite).unwrap()).unwrap().loop_count, 0);
    }

    #[test]
    fn parses_libavif_auxiliary_alpha_sequence() {
        if !ToolRunner::default().avifenc_available() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.png");
        let second = directory.path().join("second.png");
        let output = directory.path().join("alpha.avif");
        ImageBuffer::from_pixel(16, 8, Rgba([10_u8, 20, 30, 64]))
            .save(&first)
            .unwrap();
        ImageBuffer::from_pixel(16, 8, Rgba([80_u8, 90, 100, 160]))
            .save(&second)
            .unwrap();
        ToolRunner::default()
            .avifenc([
                OsString::from("-q"),
                OsString::from("90"),
                OsString::from("--qalpha"),
                OsString::from("100"),
                OsString::from("--timescale"),
                OsString::from("10"),
                OsString::from("--repetition-count"),
                OsString::from("6"),
                OsString::from("--duration"),
                OsString::from("3"),
                first.into_os_string(),
                OsString::from("--duration"),
                OsString::from("1"),
                second.into_os_string(),
                output.as_os_str().to_owned(),
            ])
            .unwrap();

        assert_eq!(
            parse(&fs::read(output).unwrap()).unwrap(),
            AnimationInfo {
                width: 16,
                height: 8,
                timescale: 10,
                loop_count: 7,
                frame_durations: vec![3, 1],
                has_alpha: true,
            }
        );
    }
}
