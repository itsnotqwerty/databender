use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
};

use crate::{
    codecs::{encoded_video, matroska_blocks, wav},
    ffmpeg::{BasicMetadata, DemuxedVideoStream, ToolRunner, VideoProperties, VideoStreamInfo},
    filters::{
        audio::compile_audio_graph, image::apply as apply_image_filters,
        video::compile_video_graph, FilterDomain, VideoPacketFrameType,
    },
    seed::{derive_seed, SeedIdentity},
    CancellationToken, DatabenderError, MediaFormat, PipelineStage, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<PathBuf> {
    let output_format = prepared.plan().format;
    debug_assert!(matches!(
        output_format,
        MediaFormat::Mp4 | MediaFormat::Matroska
    ));
    let runner = ToolRunner::default().with_cancellation(prepared.cancellation().clone());
    let expected_audio = runner
        .probe_audio_streams(prepared.input())?
        .into_iter()
        .map(|stream| stream.properties)
        .collect::<Vec<_>>();
    let video_info = runner.probe_video_streams(prepared.input())?;
    let expected_video = video_info.clone();
    let expected_metadata = runner.probe_basic_metadata(prepared.input())?;
    let expected_auxiliary = (output_format == MediaFormat::Matroska)
        .then(|| runner.probe_matroska_auxiliary(prepared.input()))
        .transpose()?;
    let (audio_stages, video_stages, packet_stages) = partition_stages(&prepared.plan().stages)?;
    let selected_video = selected_video_streams(prepared.video_streams(), video_info.len())?;
    if expected_audio.is_empty() && !audio_stages.is_empty() {
        return Err(DatabenderError::OutputValidation {
            reason: "MP4 input has no audio streams to filter".to_owned(),
        });
    }
    if !packet_stages.is_empty() {
        if !audio_stages.is_empty() || !video_stages.is_empty() {
            return Err(DatabenderError::OutputValidation {
                reason: "encoded video packet mutation cannot yet be mixed with decoded audio or video stages".to_owned(),
            });
        }
        return execute_packet_stages(
            prepared,
            runner,
            selected_video,
            packet_stages,
            PacketValidation {
                expected_audio,
                expected_video,
                expected_metadata,
                expected_auxiliary,
                expected_format: output_format,
            },
        );
    }
    let input = prepared.input().to_path_buf();
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: prepared.output().to_path_buf(),
        source,
    })?;
    let processed_video = if video_stages.is_empty() {
        Vec::new()
    } else {
        selected_video
            .iter()
            .map(|&stream_index| {
                process_video_stages(
                    &runner,
                    &input,
                    VideoStreamContext {
                        index: stream_index,
                        info: &video_info[stream_index],
                        file_seed: prepared.plan().seed,
                    },
                    &video_stages,
                    workspace.path(),
                    prepared.cancellation(),
                )
                .map(|path| (stream_index, path))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let processed_audio = if audio_stages.is_empty() {
        Vec::new()
    } else {
        expected_audio
            .iter()
            .enumerate()
            .map(|(index, _)| {
                process_audio_stages(
                    &runner,
                    &input,
                    index,
                    prepared.plan().seed,
                    &audio_stages,
                    workspace.path(),
                )
            })
            .collect::<Result<Vec<_>>>()?
    };
    let encoder = runner.clone();
    let encoded_metadata = expected_metadata.clone();

    prepared.publish_path_with(
        move |candidate| {
            let arguments = encode_arguments(
                &input,
                candidate,
                video_info.len(),
                &processed_video,
                &processed_audio,
                &encoded_metadata,
                output_format,
            );
            encoder.ffmpeg(arguments)?;
            drop(workspace);
            Ok(())
        },
        move |candidate| {
            validate(
                candidate,
                &runner,
                &expected_audio,
                &expected_video,
                &expected_metadata,
                expected_auxiliary.as_ref(),
                output_format,
            )
        },
    )
}

fn selected_video_streams(requested: &[usize], stream_count: usize) -> Result<Vec<usize>> {
    let selected = if requested.is_empty() {
        vec![0]
    } else {
        requested.to_vec()
    };
    let mut unique = HashSet::with_capacity(selected.len());
    for &index in &selected {
        if index >= stream_count {
            return Err(DatabenderError::OutputValidation {
                reason: format!(
                    "video stream index {index} is out of range for {stream_count} video streams"
                ),
            });
        }
        if !unique.insert(index) {
            return Err(DatabenderError::OutputValidation {
                reason: format!("video stream index {index} was selected more than once"),
            });
        }
    }
    Ok(selected)
}

fn partition_stages(
    stages: &[PipelineStage],
) -> Result<(Vec<PipelineStage>, Vec<PipelineStage>, Vec<PipelineStage>)> {
    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut packets = Vec::new();
    for stage in stages {
        match stage.domain {
            FilterDomain::PcmAudio | FilterDomain::FfmpegAudio => audio.push(stage.clone()),
            FilterDomain::ImagePixels | FilterDomain::FfmpegVideo => video.push(stage.clone()),
            FilterDomain::EncodedVideoPacket => packets.push(stage.clone()),
            domain => {
                return Err(DatabenderError::OutputValidation {
                    reason: format!("MP4 executor cannot process the {domain:?} domain"),
                });
            }
        }
    }
    Ok((audio, video, packets))
}

struct PacketValidation {
    expected_audio: Vec<crate::ffmpeg::AudioProperties>,
    expected_video: Vec<VideoStreamInfo>,
    expected_metadata: BasicMetadata,
    expected_auxiliary: Option<crate::ffmpeg::MatroskaAuxiliary>,
    expected_format: MediaFormat,
}

fn execute_packet_stages(
    prepared: PreparedTransform,
    runner: ToolRunner,
    selected_video: Vec<usize>,
    stages: Vec<PipelineStage>,
    validation: PacketValidation,
) -> Result<PathBuf> {
    let input = prepared.input().to_path_buf();
    let mut streams = selected_video
        .iter()
        .map(|stream| runner.probe_video_packets(&input, *stream))
        .collect::<Result<Vec<_>>>()?;
    if validation.expected_format == MediaFormat::Matroska {
        for stream in &mut streams {
            stream.packets = matroska_blocks::resolve_packet_positions(&input, &stream.packets)?;
        }
    }
    validate_packet_ranges(&streams)?;
    let max_frame_loss = stages
        .iter()
        .flat_map(|stage| &stage.filters)
        .filter_map(|filter| match filter {
            crate::FilterSpec::VideoPacketNoise { max_frame_loss, .. } => Some(*max_frame_loss),
            _ => None,
        })
        .min()
        .unwrap_or(0);
    let writer_runner = runner.clone();
    let writer_input = input.clone();
    let writer_streams = streams.clone();
    let validation_selected = selected_video.clone();

    prepared.publish_path_with(
        move |candidate| {
            fs::copy(&writer_input, candidate).map_err(|source| io_error(candidate, source))?;
            apply_packet_stages(candidate, &writer_runner, &writer_streams, &stages)
        },
        move |candidate| {
            validate_packet_candidate(
                candidate,
                &runner,
                &validation,
                &validation_selected,
                max_frame_loss,
            )
        },
    )
}

fn validate_packet_ranges(streams: &[DemuxedVideoStream]) -> Result<()> {
    let mut ranges = streams
        .iter()
        .flat_map(|stream| &stream.packets)
        .map(|packet| {
            let start = packet
                .position
                .ok_or_else(|| DatabenderError::OutputValidation {
                    reason: "video packet omitted its file position".to_owned(),
                })?;
            let end = start.checked_add(packet.size as u64).ok_or_else(|| {
                DatabenderError::OutputValidation {
                    reason: "video packet range overflows".to_owned(),
                }
            })?;
            Ok((start, end))
        })
        .collect::<Result<Vec<_>>>()?;
    ranges.sort_unstable();
    if ranges.windows(2).any(|ranges| ranges[0].1 > ranges[1].0) {
        return Err(DatabenderError::OutputValidation {
            reason: "video packet file ranges overlap".to_owned(),
        });
    }
    Ok(())
}

fn validate_packet_candidate(
    candidate: &Path,
    runner: &ToolRunner,
    validation: &PacketValidation,
    selected_video: &[usize],
    max_frame_loss: usize,
) -> Result<()> {
    if MediaFormat::detect(candidate)? != validation.expected_format {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "encoded packet candidate is not a {} container",
                validation.expected_format
            ),
        });
    }
    runner.validate_audio_streams(candidate, &validation.expected_audio)?;
    let actual_video = runner.probe_video_streams(candidate)?;
    if actual_video.len() != validation.expected_video.len() {
        return Err(DatabenderError::OutputValidation {
            reason: format!(
                "video stream count changed from {} to {}",
                validation.expected_video.len(),
                actual_video.len()
            ),
        });
    }
    for (index, (actual, expected)) in actual_video
        .iter()
        .zip(&validation.expected_video)
        .enumerate()
    {
        if actual.codec_name != expected.codec_name
            || actual.properties != expected.properties
            || actual.frame_rate != expected.frame_rate
        {
            return Err(DatabenderError::OutputValidation {
                reason: format!("video stream {index} codec, dimensions, or frame rate changed"),
            });
        }
        let allowed_loss = if selected_video.contains(&index) {
            max_frame_loss as u64
        } else {
            0
        };
        if actual.frame_count > expected.frame_count
            || expected.frame_count - actual.frame_count > allowed_loss
        {
            return Err(DatabenderError::OutputValidation {
                reason: format!(
                    "video stream {index} frame count changed from {} to {}, exceeding the allowed loss of {allowed_loss}",
                    expected.frame_count, actual.frame_count
                ),
            });
        }
    }
    if runner.probe_basic_metadata(candidate)? != validation.expected_metadata {
        return Err(DatabenderError::OutputValidation {
            reason: "basic container metadata changed during encoded packet mutation".to_owned(),
        });
    }
    if let Some(expected) = &validation.expected_auxiliary {
        if runner.probe_matroska_auxiliary(candidate)? != *expected {
            return Err(DatabenderError::OutputValidation {
                reason: "Matroska subtitles, attachments, or chapters changed during encoded packet mutation".to_owned(),
            });
        }
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        candidate.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v?"),
        OsString::from("-map"),
        OsString::from("0:a?"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ])?;
    Ok(())
}

fn apply_packet_stages(
    candidate: &Path,
    runner: &ToolRunner,
    streams: &[DemuxedVideoStream],
    stages: &[PipelineStage],
) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(candidate)
        .map_err(|source| io_error(candidate, source))?;
    let mut modified = HashSet::new();
    for stage in stages {
        for (filter_index, filter) in stage.filters.iter().enumerate() {
            let crate::FilterSpec::VideoPacketNoise {
                byte_budget,
                start_packet,
                packet_count,
                frame_type,
                intensity,
                max_frame_loss: _,
            } = filter
            else {
                unreachable!("encoded video packet stages contain packet filters")
            };
            for stream in streams {
                let end = if *packet_count == 0 {
                    stream.packets.len()
                } else {
                    start_packet
                        .saturating_add(*packet_count)
                        .min(stream.packets.len())
                };
                if *start_packet >= end {
                    return Err(DatabenderError::OutputValidation {
                        reason: format!(
                            "video packet target starts at {start_packet}, but stream {} has {} packets",
                            stream.stream_index,
                            stream.packets.len()
                        ),
                    });
                }
                let mut matched = 0;
                for (packet_index, packet) in stream.packets[*start_packet..end].iter().enumerate()
                {
                    if !frame_type_matches(*frame_type, packet.keyframe) {
                        continue;
                    }
                    matched += 1;
                    let position = packet.position.expect("packet ranges were validated");
                    let key = (position, packet.size);
                    let mut encoded = if modified.contains(&key) {
                        read_packet_at(&mut file, candidate, position, packet.size)?
                    } else {
                        runner.read_video_packet(candidate, packet)?
                    };
                    let stream_seed = derive_seed(
                        stage.seed.wrapping_add(filter_index as u64),
                        SeedIdentity::VideoStream(stream.stream_index as u64),
                    );
                    let packet_seed = derive_seed(
                        stream_seed,
                        SeedIdentity::Packet((*start_packet + packet_index) as u64),
                    );
                    encoded_video::mutate_packet(
                        &stream.codec.codec_name,
                        &mut encoded,
                        stream.codec.nal_length_size,
                        *byte_budget,
                        *intensity,
                        packet_seed,
                    )?;
                    file.seek(SeekFrom::Start(position))
                        .and_then(|_| file.write_all(&encoded))
                        .map_err(|source| io_error(candidate, source))?;
                    modified.insert(key);
                }
                if matched == 0 {
                    return Err(DatabenderError::OutputValidation {
                        reason: format!(
                            "video packet target selected no {frame_type} packets in stream {}",
                            stream.stream_index
                        ),
                    });
                }
            }
        }
    }
    file.sync_all()
        .map_err(|source| io_error(candidate, source))
}

fn frame_type_matches(frame_type: VideoPacketFrameType, keyframe: bool) -> bool {
    match frame_type {
        VideoPacketFrameType::All => true,
        VideoPacketFrameType::Key => keyframe,
        VideoPacketFrameType::Delta => !keyframe,
    }
}

fn read_packet_at(file: &mut File, path: &Path, position: u64, size: usize) -> Result<Vec<u8>> {
    let mut encoded = vec![0; size];
    file.seek(SeekFrom::Start(position))
        .and_then(|_| file.read_exact(&mut encoded))
        .map_err(|source| io_error(path, source))?;
    Ok(encoded)
}

pub(crate) fn process_audio_stages(
    runner: &ToolRunner,
    input: &Path,
    stream_index: usize,
    file_seed: u64,
    stages: &[PipelineStage],
    workspace: &Path,
) -> Result<PathBuf> {
    let mut current = input.to_path_buf();
    let mut selector = format!("0:a:{stream_index}");
    for (stage_index, stage) in stages.iter().enumerate() {
        runner.check_cancelled()?;
        let output = workspace.join(format!("audio-{stream_index}-{stage_index}.wav"));
        let mut arguments = vec![
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            current.as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from(&selector),
        ];
        if stage.domain == FilterDomain::FfmpegAudio {
            arguments.extend([
                OsString::from("-af"),
                OsString::from(compile_audio_graph(&stage.filters)?),
            ]);
        }
        arguments.extend([
            OsString::from("-codec:a"),
            OsString::from("pcm_s16le"),
            OsString::from("-f"),
            OsString::from("wav"),
            output.as_os_str().to_owned(),
        ]);
        runner.ffmpeg(arguments)?;

        if stage.domain == FilterDomain::PcmAudio {
            let mut encoded = read_file(&output)?;
            let stream_seed =
                derive_seed(file_seed, SeedIdentity::AudioStream(stream_index as u64));
            wav::apply_pcm_stage(
                &mut encoded,
                &stage.filters,
                derive_seed(stream_seed, SeedIdentity::Stage(stage.index)),
            )?;
            write_file(&output, &encoded)?;
        }
        current = output;
        selector = "0:a:0".to_owned();
    }
    Ok(current)
}

#[derive(Clone, Copy)]
struct VideoStreamContext<'a> {
    index: usize,
    info: &'a VideoStreamInfo,
    file_seed: u64,
}

fn process_video_stages(
    runner: &ToolRunner,
    input: &Path,
    stream: VideoStreamContext<'_>,
    stages: &[PipelineStage],
    workspace: &Path,
    cancellation: &CancellationToken,
) -> Result<PathBuf> {
    let mut current = input.to_path_buf();
    let mut selector = format!("0:v:{}", stream.index);
    let stream_seed = derive_seed(
        stream.file_seed,
        SeedIdentity::VideoStream(stream.index as u64),
    );
    for (stage_index, stage) in stages.iter().enumerate() {
        cancellation.check()?;
        let output = workspace.join(format!("video-{}-{stage_index}.mkv", stream.index));
        match stage.domain {
            FilterDomain::FfmpegVideo => {
                runner.ffmpeg([
                    OsString::from("-v"),
                    OsString::from("error"),
                    OsString::from("-y"),
                    OsString::from("-i"),
                    current.as_os_str().to_owned(),
                    OsString::from("-map"),
                    OsString::from(&selector),
                    OsString::from("-vf"),
                    OsString::from(compile_video_graph(&stage.filters)?),
                    OsString::from("-an"),
                    OsString::from("-codec:v"),
                    OsString::from("ffv1"),
                    OsString::from("-level"),
                    OsString::from("3"),
                    OsString::from("-f"),
                    OsString::from("matroska"),
                    output.as_os_str().to_owned(),
                ])?;
            }
            FilterDomain::ImagePixels => {
                let frame_rate = stream.info.frame_rate.as_deref().ok_or_else(|| {
                    DatabenderError::OutputValidation {
                        reason: "FFprobe did not report a usable video frame rate".to_owned(),
                    }
                })?;
                let decoded = workspace.join(format!("video-{stage_index}-decoded.rgba"));
                let filtered = workspace.join(format!("video-{stage_index}-filtered.rgba"));
                runner.ffmpeg([
                    OsString::from("-v"),
                    OsString::from("error"),
                    OsString::from("-y"),
                    OsString::from("-i"),
                    current.as_os_str().to_owned(),
                    OsString::from("-map"),
                    OsString::from(&selector),
                    OsString::from("-an"),
                    OsString::from("-pix_fmt"),
                    OsString::from("rgba"),
                    OsString::from("-f"),
                    OsString::from("rawvideo"),
                    decoded.as_os_str().to_owned(),
                ])?;
                filter_raw_frames(
                    &decoded,
                    &filtered,
                    stream.info.properties,
                    stage,
                    derive_seed(stream_seed, SeedIdentity::Stage(stage.index)),
                    cancellation,
                )?;
                runner.ffmpeg([
                    OsString::from("-v"),
                    OsString::from("error"),
                    OsString::from("-y"),
                    OsString::from("-f"),
                    OsString::from("rawvideo"),
                    OsString::from("-pixel_format"),
                    OsString::from("rgba"),
                    OsString::from("-video_size"),
                    OsString::from(format!(
                        "{}x{}",
                        stream.info.properties.width, stream.info.properties.height
                    )),
                    OsString::from("-framerate"),
                    OsString::from(frame_rate),
                    OsString::from("-i"),
                    filtered.as_os_str().to_owned(),
                    OsString::from("-an"),
                    OsString::from("-codec:v"),
                    OsString::from("ffv1"),
                    OsString::from("-level"),
                    OsString::from("3"),
                    OsString::from("-f"),
                    OsString::from("matroska"),
                    output.as_os_str().to_owned(),
                ])?;
            }
            domain => {
                return Err(DatabenderError::OutputValidation {
                    reason: format!("MP4 video pipeline cannot process the {domain:?} domain"),
                });
            }
        }
        current = output;
        selector = "0:v:0".to_owned();
    }
    Ok(current)
}

fn filter_raw_frames(
    input: &Path,
    output: &Path,
    properties: VideoProperties,
    stage: &PipelineStage,
    stage_seed: u64,
    cancellation: &CancellationToken,
) -> Result<()> {
    let workers = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(8);
    filter_raw_frames_with_workers(
        input,
        output,
        properties,
        stage,
        stage_seed,
        workers,
        cancellation,
    )
}

fn filter_raw_frames_with_workers(
    input: &Path,
    output: &Path,
    properties: VideoProperties,
    stage: &PipelineStage,
    stage_seed: u64,
    workers: usize,
    cancellation: &CancellationToken,
) -> Result<()> {
    let frame_bytes = (properties.width as usize)
        .checked_mul(properties.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "video frame dimensions overflow the address space".to_owned(),
        })?;
    let mut reader = BufReader::new(open_file(input)?);
    let mut writer = BufWriter::new(create_file(output)?);
    let mut frame_index = 0_u64;
    loop {
        cancellation.check()?;
        let mut batch = Vec::with_capacity(workers.max(1));
        for _ in 0..workers.max(1) {
            match read_raw_frame(&mut reader, input, frame_bytes)? {
                Some(frame) => batch.push(frame),
                None => break,
            }
        }
        if batch.is_empty() {
            writer.flush().map_err(|source| io_error(output, source))?;
            return Ok(());
        }
        let filtered = thread::scope(|scope| {
            let handles = batch
                .into_iter()
                .enumerate()
                .map(|(offset, mut frame)| {
                    let cancellation = cancellation.clone();
                    scope.spawn(move || {
                        cancellation.check()?;
                        apply_image_filters(
                            &stage.filters,
                            &mut frame,
                            properties.width,
                            properties.height,
                            derive_seed(
                                stage_seed,
                                SeedIdentity::Frame(frame_index + offset as u64),
                            ),
                        )?;
                        Ok(frame)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| DatabenderError::OutputValidation {
                            reason: "video frame worker panicked".to_owned(),
                        })?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for frame in &filtered {
            writer
                .write_all(frame)
                .map_err(|source| io_error(output, source))?;
        }
        frame_index = frame_index.wrapping_add(filtered.len() as u64);
    }
}

fn read_raw_frame(
    reader: &mut BufReader<File>,
    input: &Path,
    frame_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    let mut frame = vec![0_u8; frame_bytes];
    let mut read = 0;
    while read < frame.len() {
        let count = reader
            .read(&mut frame[read..])
            .map_err(|source| io_error(input, source))?;
        if count == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(DatabenderError::OutputValidation {
                reason: format!("decoded video ended with a partial RGBA frame ({read} bytes)"),
            });
        }
        read += count;
    }
    Ok(Some(frame))
}

fn encode_arguments(
    input: &Path,
    candidate: &Path,
    video_stream_count: usize,
    processed_video: &[(usize, PathBuf)],
    processed_audio: &[PathBuf],
    metadata: &BasicMetadata,
    output_format: MediaFormat,
) -> Vec<OsString> {
    let mut arguments = vec![
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
    ];
    let mut next_input = 1;
    let mut processed_video_selectors = Vec::with_capacity(processed_video.len());
    for (stream_index, video) in processed_video {
        arguments.extend([OsString::from("-i"), video.as_os_str().to_owned()]);
        processed_video_selectors.push((*stream_index, format!("{next_input}:v:0")));
        next_input += 1;
    }
    let mut audio_selectors = Vec::new();
    for audio in processed_audio {
        arguments.extend([OsString::from("-i"), audio.as_os_str().to_owned()]);
        audio_selectors.push(format!("{next_input}:a:0"));
        next_input += 1;
    }
    for stream_index in 0..video_stream_count {
        let selector = processed_video_selectors
            .iter()
            .find_map(|(index, selector)| (*index == stream_index).then_some(selector.as_str()))
            .map_or_else(|| format!("0:v:{stream_index}"), str::to_owned);
        arguments.extend([OsString::from("-map"), OsString::from(selector)]);
    }
    if audio_selectors.is_empty() {
        arguments.extend([OsString::from("-map"), OsString::from("0:a?")]);
    } else {
        for selector in audio_selectors {
            arguments.extend([OsString::from("-map"), OsString::from(selector)]);
        }
    }
    if output_format == MediaFormat::Matroska {
        arguments.extend([
            OsString::from("-map"),
            OsString::from("0:s?"),
            OsString::from("-map"),
            OsString::from("0:t?"),
            OsString::from("-map_chapters"),
            OsString::from("0"),
            OsString::from("-codec:s"),
            OsString::from("copy"),
            OsString::from("-codec:t"),
            OsString::from("copy"),
        ]);
    }
    arguments.extend([
        OsString::from("-map_metadata"),
        OsString::from("0"),
        OsString::from("-codec:v"),
        OsString::from("copy"),
    ]);
    for (stream_index, _) in processed_video {
        arguments.extend([
            OsString::from(format!("-codec:v:{stream_index}")),
            OsString::from(match output_format {
                MediaFormat::Mp4 => "mpeg4",
                MediaFormat::Matroska => "ffv1",
                _ => unreachable!("adapter accepts MP4 or Matroska"),
            }),
        ]);
        if output_format == MediaFormat::Mp4 {
            arguments.extend([
                OsString::from(format!("-q:v:{stream_index}")),
                OsString::from("3"),
            ]);
        }
    }
    arguments.extend([
        OsString::from("-codec:a"),
        OsString::from(if processed_audio.is_empty() {
            "copy"
        } else {
            match output_format {
                MediaFormat::Mp4 => "aac",
                MediaFormat::Matroska => "flac",
                _ => unreachable!("adapter accepts MP4 or Matroska"),
            }
        }),
    ]);
    if !processed_audio.is_empty() && output_format == MediaFormat::Mp4 {
        arguments.extend([OsString::from("-b:a"), OsString::from("192k")]);
    }
    append_metadata(&mut arguments, "-metadata", &metadata.format);
    for (index, video) in metadata.video.iter().enumerate() {
        append_metadata(&mut arguments, &format!("-metadata:s:v:{index}"), video);
    }
    for (index, audio) in metadata.audio.iter().enumerate() {
        append_metadata(&mut arguments, &format!("-metadata:s:a:{index}"), audio);
    }
    if output_format == MediaFormat::Mp4 {
        arguments.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
    }
    arguments.extend([
        OsString::from("-f"),
        OsString::from(match output_format {
            MediaFormat::Mp4 => "mp4",
            MediaFormat::Matroska => "matroska",
            _ => unreachable!("adapter accepts MP4 or Matroska"),
        }),
        candidate.as_os_str().to_owned(),
    ]);
    arguments
}

fn append_metadata(
    arguments: &mut Vec<OsString>,
    option: &str,
    metadata: &std::collections::BTreeMap<String, String>,
) {
    for (key, value) in metadata {
        arguments.extend([
            OsString::from(option),
            OsString::from(format!("{key}={value}")),
        ]);
    }
}

fn validate(
    candidate: &Path,
    runner: &ToolRunner,
    expected_audio: &[crate::ffmpeg::AudioProperties],
    expected_video: &[VideoStreamInfo],
    expected_metadata: &BasicMetadata,
    expected_auxiliary: Option<&crate::ffmpeg::MatroskaAuxiliary>,
    expected_format: MediaFormat,
) -> Result<()> {
    if MediaFormat::detect(candidate)? != expected_format {
        return Err(DatabenderError::OutputValidation {
            reason: format!("FFmpeg candidate is not a {expected_format} container"),
        });
    }
    runner.validate_audio_streams(candidate, expected_audio)?;
    runner.validate_video_streams(candidate, expected_video)?;
    let actual_metadata = runner.probe_basic_metadata(candidate)?;
    if actual_metadata != *expected_metadata {
        return Err(DatabenderError::OutputValidation {
            reason: "basic MP4 metadata changed during transformation".to_owned(),
        });
    }
    if let Some(expected) = expected_auxiliary {
        let actual = runner.probe_matroska_auxiliary(candidate)?;
        if actual != *expected {
            return Err(DatabenderError::OutputValidation {
                reason: "Matroska subtitles, attachments, or chapters changed".to_owned(),
            });
        }
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        candidate.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v?"),
        OsString::from("-map"),
        OsString::from("0:a?"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ])?;
    Ok(())
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| io_error(path, source))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).map_err(|source| io_error(path, source))
}

fn open_file(path: &Path) -> Result<File> {
    File::open(path).map_err(|source| io_error(path, source))
}

fn create_file(path: &Path) -> Result<File> {
    File::create(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: std::io::Error) -> DatabenderError {
    DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilterSpec, StreamKind};

    #[test]
    fn parallel_frame_filtering_matches_single_worker_output() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.rgba");
        let serial = directory.path().join("serial.rgba");
        let parallel = directory.path().join("parallel.rgba");
        let encoded = (0..8 * 4 * 2 * 4)
            .map(|value| (value % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&input, encoded).unwrap();
        let stage = PipelineStage {
            domain: FilterDomain::ImagePixels,
            target: StreamKind::Image,
            seed: 42,
            filters: vec![FilterSpec::RowDropout { probability: 0.5 }],
            resolved_graph: None,
            environment_dependent: false,
            index: 0,
        };
        let properties = VideoProperties {
            width: 4,
            height: 2,
        };

        let cancellation = CancellationToken::default();
        let stage_seed = derive_seed(
            derive_seed(42, SeedIdentity::VideoStream(0)),
            SeedIdentity::Stage(0),
        );
        filter_raw_frames_with_workers(
            &input,
            &serial,
            properties,
            &stage,
            stage_seed,
            1,
            &cancellation,
        )
        .unwrap();
        filter_raw_frames_with_workers(
            &input,
            &parallel,
            properties,
            &stage,
            stage_seed,
            4,
            &cancellation,
        )
        .unwrap();

        assert_eq!(fs::read(serial).unwrap(), fs::read(parallel).unwrap());
    }

    #[test]
    fn video_stream_identity_changes_frame_seed() {
        let frame_seed = |stream_index| {
            let stream = derive_seed(42, SeedIdentity::VideoStream(stream_index));
            let stage = derive_seed(stream, SeedIdentity::Stage(3));
            derive_seed(stage, SeedIdentity::Frame(7))
        };

        assert_ne!(frame_seed(0), frame_seed(1));
        assert_eq!(frame_seed(0), frame_seed(0));
    }
}
