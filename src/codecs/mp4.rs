use std::{
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    codecs::wav,
    ffmpeg::{BasicMetadata, ToolRunner, VideoProperties},
    filters::{
        audio::compile_audio_graph, image::apply as apply_image_filters,
        video::compile_video_graph, FilterDomain,
    },
    DatabenderError, MediaFormat, PipelineStage, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<PathBuf> {
    let output_format = prepared.plan().format;
    debug_assert!(matches!(
        output_format,
        MediaFormat::Mp4 | MediaFormat::Matroska
    ));
    let runner = ToolRunner::default();
    let expected_audio = runner
        .probe_audio_streams(prepared.input())?
        .into_iter()
        .map(|stream| stream.properties)
        .collect::<Vec<_>>();
    let video_info = runner.probe_video(prepared.input())?;
    let expected_video = video_info.properties;
    let expected_metadata = runner.probe_basic_metadata(prepared.input())?;
    let (audio_stages, video_stages) = partition_stages(&prepared.plan().stages)?;
    if expected_audio.is_empty() && !audio_stages.is_empty() {
        return Err(DatabenderError::OutputValidation {
            reason: "MP4 input has no audio streams to filter".to_owned(),
        });
    }
    let input = prepared.input().to_path_buf();
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: prepared.output().to_path_buf(),
        source,
    })?;
    let processed_video = process_video_stages(
        &runner,
        &input,
        &video_stages,
        expected_video,
        video_info.frame_rate.as_deref(),
        workspace.path(),
    )?;
    let processed_audio = if audio_stages.is_empty() {
        Vec::new()
    } else {
        expected_audio
            .iter()
            .enumerate()
            .map(|(index, _)| {
                process_audio_stages(&runner, &input, index, &audio_stages, workspace.path())
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
                processed_video.as_deref(),
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
                expected_video,
                &expected_metadata,
                output_format,
            )
        },
    )
}

fn partition_stages(stages: &[PipelineStage]) -> Result<(Vec<PipelineStage>, Vec<PipelineStage>)> {
    let mut audio = Vec::new();
    let mut video = Vec::new();
    for stage in stages {
        match stage.domain {
            FilterDomain::PcmAudio | FilterDomain::FfmpegAudio => audio.push(stage.clone()),
            FilterDomain::ImagePixels | FilterDomain::FfmpegVideo => video.push(stage.clone()),
            domain => {
                return Err(DatabenderError::OutputValidation {
                    reason: format!("MP4 executor cannot process the {domain:?} domain"),
                });
            }
        }
    }
    Ok((audio, video))
}

pub(crate) fn process_audio_stages(
    runner: &ToolRunner,
    input: &Path,
    stream_index: usize,
    stages: &[PipelineStage],
    workspace: &Path,
) -> Result<PathBuf> {
    let mut current = input.to_path_buf();
    let mut selector = format!("0:a:{stream_index}");
    for (stage_index, stage) in stages.iter().enumerate() {
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
            wav::apply_pcm_stage(
                &mut encoded,
                &stage.filters,
                stage.seed.wrapping_add(stream_index as u64),
            )?;
            write_file(&output, &encoded)?;
        }
        current = output;
        selector = "0:a:0".to_owned();
    }
    Ok(current)
}

fn process_video_stages(
    runner: &ToolRunner,
    input: &Path,
    stages: &[PipelineStage],
    properties: VideoProperties,
    frame_rate: Option<&str>,
    workspace: &Path,
) -> Result<Option<PathBuf>> {
    if stages.is_empty() {
        return Ok(None);
    }
    let mut current = input.to_path_buf();
    for (stage_index, stage) in stages.iter().enumerate() {
        let output = workspace.join(format!("video-{stage_index}.mkv"));
        match stage.domain {
            FilterDomain::FfmpegVideo => {
                runner.ffmpeg([
                    OsString::from("-v"),
                    OsString::from("error"),
                    OsString::from("-y"),
                    OsString::from("-i"),
                    current.as_os_str().to_owned(),
                    OsString::from("-map"),
                    OsString::from("0:v:0"),
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
                let frame_rate = frame_rate.ok_or_else(|| DatabenderError::OutputValidation {
                    reason: "FFprobe did not report a usable video frame rate".to_owned(),
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
                    OsString::from("0:v:0"),
                    OsString::from("-an"),
                    OsString::from("-pix_fmt"),
                    OsString::from("rgba"),
                    OsString::from("-f"),
                    OsString::from("rawvideo"),
                    decoded.as_os_str().to_owned(),
                ])?;
                filter_raw_frames(&decoded, &filtered, properties, stage)?;
                runner.ffmpeg([
                    OsString::from("-v"),
                    OsString::from("error"),
                    OsString::from("-y"),
                    OsString::from("-f"),
                    OsString::from("rawvideo"),
                    OsString::from("-pixel_format"),
                    OsString::from("rgba"),
                    OsString::from("-video_size"),
                    OsString::from(format!("{}x{}", properties.width, properties.height)),
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
    }
    Ok(Some(current))
}

fn filter_raw_frames(
    input: &Path,
    output: &Path,
    properties: VideoProperties,
    stage: &PipelineStage,
) -> Result<()> {
    let frame_bytes = (properties.width as usize)
        .checked_mul(properties.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| DatabenderError::OutputValidation {
            reason: "video frame dimensions overflow the address space".to_owned(),
        })?;
    let mut reader = BufReader::new(open_file(input)?);
    let mut writer = BufWriter::new(create_file(output)?);
    let mut frame = vec![0_u8; frame_bytes];
    let mut frame_index = 0_u64;
    loop {
        let mut read = 0;
        while read < frame.len() {
            let count = reader
                .read(&mut frame[read..])
                .map_err(|source| io_error(input, source))?;
            if count == 0 {
                if read == 0 {
                    writer.flush().map_err(|source| io_error(output, source))?;
                    return Ok(());
                }
                return Err(DatabenderError::OutputValidation {
                    reason: format!("decoded video ended with a partial RGBA frame ({read} bytes)"),
                });
            }
            read += count;
        }
        apply_image_filters(
            &stage.filters,
            &mut frame,
            properties.width,
            properties.height,
            stage.seed.wrapping_add(frame_index),
        )?;
        writer
            .write_all(&frame)
            .map_err(|source| io_error(output, source))?;
        frame_index = frame_index.wrapping_add(1);
    }
}

fn encode_arguments(
    input: &Path,
    candidate: &Path,
    processed_video: Option<&Path>,
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
    let video_selector = if let Some(video) = processed_video {
        arguments.extend([OsString::from("-i"), video.as_os_str().to_owned()]);
        next_input += 1;
        "1:v:0".to_owned()
    } else {
        "0:v:0".to_owned()
    };
    let mut audio_selectors = Vec::new();
    for audio in processed_audio {
        arguments.extend([OsString::from("-i"), audio.as_os_str().to_owned()]);
        audio_selectors.push(format!("{next_input}:a:0"));
        next_input += 1;
    }
    arguments.extend([OsString::from("-map"), OsString::from(video_selector)]);
    if audio_selectors.is_empty() {
        arguments.extend([OsString::from("-map"), OsString::from("0:a?")]);
    } else {
        for selector in audio_selectors {
            arguments.extend([OsString::from("-map"), OsString::from(selector)]);
        }
    }
    arguments.extend([
        OsString::from("-map_metadata"),
        OsString::from("0"),
        OsString::from("-codec:v"),
        OsString::from(if processed_video.is_some() {
            match output_format {
                MediaFormat::Mp4 => "mpeg4",
                MediaFormat::Matroska => "ffv1",
                _ => unreachable!("adapter accepts MP4 or Matroska"),
            }
        } else {
            "copy"
        }),
    ]);
    if processed_video.is_some() && output_format == MediaFormat::Mp4 {
        arguments.extend([OsString::from("-q:v"), OsString::from("3")]);
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
    if let Some(video) = &metadata.video {
        append_metadata(&mut arguments, "-metadata:s:v:0", video);
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
    expected_video: VideoProperties,
    expected_metadata: &BasicMetadata,
    expected_format: MediaFormat,
) -> Result<()> {
    if MediaFormat::detect(candidate)? != expected_format {
        return Err(DatabenderError::OutputValidation {
            reason: format!("FFmpeg candidate is not a {expected_format} container"),
        });
    }
    runner.validate_audio_streams(candidate, expected_audio)?;
    runner.validate_video(candidate, expected_video)?;
    let actual_metadata = runner.probe_basic_metadata(candidate)?;
    if actual_metadata != *expected_metadata {
        return Err(DatabenderError::OutputValidation {
            reason: "basic MP4 metadata changed during transformation".to_owned(),
        });
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        candidate.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
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
