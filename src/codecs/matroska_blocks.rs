use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
};

use sha2::{Digest, Sha256};

use crate::{ffmpeg::VideoPacketRecord, DatabenderError, Result};

const SEGMENT_ID: u64 = 0x1853_8067;
const CLUSTER_ID: u64 = 0x1f43_b675;
const BLOCK_GROUP_ID: u64 = 0xa0;
const SIMPLE_BLOCK_ID: u64 = 0xa3;
const BLOCK_ID: u64 = 0xa1;
const MAX_PACKET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Element {
    id: u64,
    data: u64,
    end: u64,
}

pub(crate) fn resolve_packet_positions(
    path: &Path,
    packets: &[VideoPacketRecord],
) -> Result<Vec<VideoPacketRecord>> {
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let file_len = file
        .metadata()
        .map_err(|source| io_error(path, source))?
        .len();
    let mut blocks = Vec::new();
    collect_segments(&mut file, 0, file_len, &mut blocks, path)?;

    let mut resolved = Vec::with_capacity(packets.len());
    let mut next_block = 0;
    for packet in packets {
        let packet_size = u64::try_from(packet.size)
            .map_err(|_| invalid("packet size exceeds the Matroska address space"))?;
        let mut matched = None;
        for (index, range) in blocks.iter().enumerate().skip(next_block) {
            if range.end - range.start != packet_size {
                continue;
            }
            if hash_range(&mut file, range.clone(), path)? == packet.data_hash {
                matched = Some((index, range.start));
                break;
            }
        }
        let (index, position) = matched.ok_or_else(|| {
            invalid(format!(
                "could not identify exact unlaced Matroska payload bytes for packet {}",
                resolved.len()
            ))
        })?;
        next_block = index + 1;
        let mut packet = packet.clone();
        packet.position = Some(position);
        resolved.push(packet);
    }
    Ok(resolved)
}

fn collect_segments(
    file: &mut File,
    start: u64,
    end: u64,
    blocks: &mut Vec<Range<u64>>,
    path: &Path,
) -> Result<()> {
    walk_elements(file, start, end, path, |file, element| {
        if element.id == SEGMENT_ID {
            collect_clusters(file, element.data, element.end, blocks, path)?;
        }
        Ok(())
    })
}

fn collect_clusters(
    file: &mut File,
    start: u64,
    end: u64,
    blocks: &mut Vec<Range<u64>>,
    path: &Path,
) -> Result<()> {
    walk_elements(file, start, end, path, |file, element| {
        if element.id == CLUSTER_ID {
            collect_cluster_blocks(file, element.data, element.end, blocks, path)?;
        }
        Ok(())
    })
}

fn collect_cluster_blocks(
    file: &mut File,
    start: u64,
    end: u64,
    blocks: &mut Vec<Range<u64>>,
    path: &Path,
) -> Result<()> {
    walk_elements(file, start, end, path, |file, element| {
        match element.id {
            SIMPLE_BLOCK_ID => {
                if let Some(payload) = block_payload(file, element, path)? {
                    blocks.push(payload);
                }
            }
            BLOCK_GROUP_ID => {
                walk_elements(file, element.data, element.end, path, |file, child| {
                    if child.id == BLOCK_ID {
                        if let Some(payload) = block_payload(file, child, path)? {
                            blocks.push(payload);
                        }
                    }
                    Ok(())
                })?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn walk_elements(
    file: &mut File,
    mut offset: u64,
    end: u64,
    path: &Path,
    mut visit: impl FnMut(&mut File, Element) -> Result<()>,
) -> Result<()> {
    while offset < end {
        let element = read_element(file, offset, end, path)?;
        visit(file, element)?;
        if element.end <= offset {
            return Err(invalid("Matroska element did not advance"));
        }
        offset = element.end;
    }
    Ok(())
}

fn read_element(file: &mut File, offset: u64, parent_end: u64, path: &Path) -> Result<Element> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| io_error(path, source))?;
    let (id, id_width, _) = read_vint(file, true, path)?;
    if id_width > 4 {
        return Err(invalid("Matroska element ID exceeds four bytes"));
    }
    let (size, size_width, unknown) = read_vint(file, false, path)?;
    let data = offset
        .checked_add(id_width as u64 + size_width as u64)
        .ok_or_else(|| invalid("Matroska element header overflows"))?;
    let end = if unknown {
        parent_end
    } else {
        data.checked_add(size)
            .filter(|end| *end <= parent_end)
            .ok_or_else(|| invalid("Matroska element exceeds its parent"))?
    };
    Ok(Element { id, data, end })
}

fn read_vint(file: &mut File, retain_marker: bool, path: &Path) -> Result<(u64, usize, bool)> {
    let mut first = [0_u8; 1];
    file.read_exact(&mut first)
        .map_err(|source| io_error(path, source))?;
    if first[0] == 0 {
        return Err(invalid("Matroska variable integer has no marker bit"));
    }
    let width = first[0].leading_zeros() as usize + 1;
    if width > 8 {
        return Err(invalid("Matroska variable integer exceeds eight bytes"));
    }
    let marker_mask = 1_u8 << (8 - width);
    let mut value = if retain_marker {
        u64::from(first[0])
    } else {
        u64::from(first[0] & !marker_mask)
    };
    for _ in 1..width {
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte)
            .map_err(|source| io_error(path, source))?;
        value = (value << 8) | u64::from(byte[0]);
    }
    let unknown = !retain_marker && value == (1_u64 << (7 * width)) - 1;
    Ok((value, width, unknown))
}

fn block_payload(file: &mut File, element: Element, path: &Path) -> Result<Option<Range<u64>>> {
    file.seek(SeekFrom::Start(element.data))
        .map_err(|source| io_error(path, source))?;
    let (_, track_width, _) = read_vint(file, false, path)?;
    let header_size = track_width as u64 + 3;
    let payload = element
        .data
        .checked_add(header_size)
        .filter(|payload| *payload <= element.end)
        .ok_or_else(|| invalid("Matroska block is shorter than its header"))?;
    file.seek(SeekFrom::Start(element.data + track_width as u64 + 2))
        .map_err(|source| io_error(path, source))?;
    let mut flags = [0_u8; 1];
    file.read_exact(&mut flags)
        .map_err(|source| io_error(path, source))?;
    if flags[0] & 0x06 != 0 {
        return Ok(None);
    }
    Ok(Some(payload..element.end))
}

fn hash_range(file: &mut File, range: Range<u64>, path: &Path) -> Result<String> {
    let length = range.end - range.start;
    if length > MAX_PACKET_BYTES {
        return Err(invalid(format!(
            "Matroska packet exceeds the {MAX_PACKET_BYTES}-byte hash limit"
        )));
    }
    file.seek(SeekFrom::Start(range.start))
        .map_err(|source| io_error(path, source))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        file.read_exact(&mut buffer[..count])
            .map_err(|source| io_error(path, source))?;
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(format!("SHA256:{:x}", digest.finalize()))
}

fn io_error(path: &Path, source: std::io::Error) -> DatabenderError {
    DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn invalid(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid Matroska packet layout: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(id: &[u8], data: &[u8]) -> Vec<u8> {
        assert!(data.len() < 127);
        [id, &[0x80 | data.len() as u8], data].concat()
    }

    fn fixture(payloads: &[&[u8]]) -> Vec<u8> {
        let cluster = payloads
            .iter()
            .flat_map(|payload| {
                let block = [&[0x81, 0, 0, 0x80][..], *payload].concat();
                element(&[SIMPLE_BLOCK_ID as u8], &block)
            })
            .collect::<Vec<_>>();
        element(
            &[0x18, 0x53, 0x80, 0x67],
            &element(&[0x1f, 0x43, 0xb6, 0x75], &cluster),
        )
    }

    fn packet(payload: &[u8]) -> VideoPacketRecord {
        VideoPacketRecord {
            stream_index: 0,
            pts: Some(0),
            dts: Some(0),
            duration: Some(1),
            position: Some(0),
            size: payload.len(),
            keyframe: true,
            data_hash: format!("SHA256:{:x}", Sha256::digest(payload)),
        }
    }

    #[test]
    fn resolves_exact_unlaced_payload_positions_by_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.mkv");
        let encoded = fixture(&[b"audio", b"video"]);
        std::fs::write(&path, &encoded).unwrap();

        let resolved = resolve_packet_positions(&path, &[packet(b"video")]).unwrap();

        let position = resolved[0].position.unwrap() as usize;
        assert_eq!(&encoded[position..position + 5], b"video");
    }

    #[test]
    fn rejects_hashes_that_do_not_identify_a_block_payload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.mkv");
        std::fs::write(&path, fixture(&[b"video"])).unwrap();

        assert!(resolve_packet_positions(&path, &[packet(b"other")]).is_err());
    }
}
