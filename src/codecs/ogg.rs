use std::{ffi::OsString, fs, path::PathBuf};

use crate::{
    codecs::{mp4::process_audio_stages, ogg_pages},
    ffmpeg::{AudioProperties, AudioStreamInfo, BasicMetadata, ToolOutput, ToolRunner},
    filters::FilterDomain,
    DatabenderError, FilterSpec, MediaFormat, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<PathBuf> {
    let runner = ToolRunner::default().with_cancellation(prepared.cancellation().clone());
    let streams = runner.probe_audio_streams(prepared.input())?;
    if streams.is_empty() {
        return Err(invalid_ogg("input contains no audio streams"));
    }
    for stream in &streams {
        encoder_for(&stream.codec_name)?;
    }
    let expected_properties = streams
        .iter()
        .map(|stream| stream.properties)
        .collect::<Vec<_>>();
    let expected_metadata = runner.probe_basic_metadata(prepared.input())?;
    let input = prepared.input().to_path_buf();
    let max_decode_errors = prepared
        .plan()
        .stages
        .iter()
        .flat_map(|stage| &stage.filters)
        .filter_map(|filter| match filter {
            FilterSpec::OggPacketNoise {
                max_decode_errors, ..
            } => Some(*max_decode_errors),
            _ => None,
        })
        .min()
        .unwrap_or(0);
    let stages = prepared.plan().stages.clone();
    let encoder = runner.clone();
    let expected_streams = streams.clone();
    let cancellation = prepared.cancellation().clone();

    prepared.publish_path_with(
        move |candidate| {
            let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
                path: candidate.to_path_buf(),
                source,
            })?;
            let mut current = input.clone();
            for (stage_index, stage) in stages.iter().enumerate() {
                cancellation.check()?;
                let output = workspace.path().join(format!("stage-{stage_index}.ogg"));
                match stage.domain {
                    FilterDomain::OggPacket => {
                        let mut encoded = read(&current)?;
                        for (filter_index, filter) in stage.filters.iter().enumerate() {
                            cancellation.check()?;
                            apply_packet_filter(
                                filter,
                                &mut encoded,
                                stage.seed.wrapping_add(filter_index as u64),
                            )?;
                        }
                        write(&output, &encoded)?;
                    }
                    FilterDomain::PcmAudio | FilterDomain::FfmpegAudio => {
                        let current_streams = encoder.probe_audio_streams(&current)?;
                        let processed = current_streams
                            .iter()
                            .enumerate()
                            .map(|(index, _)| {
                                process_audio_stages(
                                    &encoder,
                                    &current,
                                    index,
                                    stage.seed,
                                    std::slice::from_ref(stage),
                                    workspace.path(),
                                )
                            })
                            .collect::<Result<Vec<_>>>()?;
                        encoder.ffmpeg(encode_arguments(
                            &current,
                            &output,
                            &processed,
                            &current_streams,
                        )?)?;
                    }
                    _ => unreachable!("Ogg plans contain only packet and audio stages"),
                }
                current = output;
            }
            fs::copy(&current, candidate).map_err(|source| DatabenderError::Io {
                path: candidate.to_path_buf(),
                source,
            })?;
            drop(workspace);
            Ok(())
        },
        move |candidate| {
            validate(
                candidate,
                &runner,
                &expected_streams,
                &expected_properties,
                &expected_metadata,
                max_decode_errors,
            )
        },
    )
}

fn apply_packet_filter(filter: &FilterSpec, encoded: &mut [u8], seed: u64) -> Result<()> {
    let FilterSpec::OggPacketNoise {
        byte_budget,
        start_packet,
        packet_count,
        intensity,
        ..
    } = filter
    else {
        return Err(invalid_ogg(format!(
            "filter {} is not an Ogg packet filter",
            filter.name()
        )));
    };
    ogg_pages::mutate(
        encoded,
        *byte_budget,
        *start_packet,
        *packet_count,
        *intensity,
        seed,
    )?;
    Ok(())
}

fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write(path: &std::path::Path, encoded: &[u8]) -> Result<()> {
    fs::write(path, encoded).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn encode_arguments(
    input: &std::path::Path,
    candidate: &std::path::Path,
    processed: &[PathBuf],
    streams: &[AudioStreamInfo],
) -> Result<Vec<OsString>> {
    let mut arguments = vec![
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        input.as_os_str().to_owned(),
    ];
    for audio in processed {
        arguments.extend([OsString::from("-i"), audio.as_os_str().to_owned()]);
    }
    for (index, stream) in streams.iter().enumerate() {
        arguments.extend([
            OsString::from("-map"),
            OsString::from(format!("{}:a:0", index + 1)),
            OsString::from(format!("-codec:a:{index}")),
            OsString::from(encoder_for(&stream.codec_name)?),
            OsString::from(format!("-map_metadata:s:a:{index}")),
            OsString::from(format!("0:s:a:{index}")),
        ]);
    }
    arguments.extend([
        OsString::from("-map_metadata"),
        OsString::from("0"),
        OsString::from("-f"),
        OsString::from("ogg"),
        candidate.as_os_str().to_owned(),
    ]);
    Ok(arguments)
}

fn encoder_for(codec: &str) -> Result<&'static str> {
    match codec {
        "vorbis" => Ok("libvorbis"),
        "opus" => Ok("libopus"),
        _ => Err(invalid_ogg(format!(
            "unsupported Ogg audio codec {codec}; expected Vorbis or Opus"
        ))),
    }
}

fn validate(
    candidate: &std::path::Path,
    runner: &ToolRunner,
    expected_streams: &[AudioStreamInfo],
    expected_properties: &[AudioProperties],
    expected_metadata: &BasicMetadata,
    max_decode_errors: usize,
) -> Result<()> {
    if MediaFormat::detect(candidate)? != MediaFormat::Ogg {
        return Err(invalid_ogg("candidate container changed"));
    }
    ogg_pages::parse(&read(candidate)?)?;
    let streams = runner.validate_audio_streams(candidate, expected_properties)?;
    for (actual, expected) in streams.iter().zip(expected_streams) {
        if actual.codec_name != expected.codec_name {
            return Err(invalid_ogg(format!(
                "audio codec changed from {} to {}",
                expected.codec_name, actual.codec_name
            )));
        }
    }
    if runner.probe_basic_metadata(candidate)? != *expected_metadata {
        return Err(invalid_ogg("basic metadata changed during transformation"));
    }
    let decode = runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        candidate.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:a"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ])?;
    enforce_damage_limit(&decode, max_decode_errors)
}

fn enforce_damage_limit(output: &ToolOutput, max_decode_errors: usize) -> Result<()> {
    if output.stderr.truncated {
        return Err(invalid_ogg(
            "decoder diagnostics were truncated, so the damage limit cannot be verified",
        ));
    }
    let decode_errors = output
        .stderr
        .bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()))
        .count();
    if decode_errors > max_decode_errors {
        return Err(invalid_ogg(format!(
            "decoder reported {decode_errors} error lines, exceeding the configured limit of {max_decode_errors}"
        )));
    }
    Ok(())
}

fn invalid_ogg(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid Ogg: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::CapturedOutput;

    fn output(stderr: &[u8], truncated: bool) -> ToolOutput {
        ToolOutput {
            stdout: CapturedOutput {
                bytes: Vec::new(),
                truncated: false,
            },
            stderr: CapturedOutput {
                bytes: stderr.to_vec(),
                truncated,
            },
        }
    }

    #[test]
    fn enforces_configured_decoder_error_lines() {
        assert!(enforce_damage_limit(&output(b"", false), 0).is_ok());
        assert!(enforce_damage_limit(&output(b"first\nsecond\n", false), 2).is_ok());
        assert!(enforce_damage_limit(&output(b"first\nsecond\n", false), 1).is_err());
        assert!(enforce_damage_limit(&output(b"first\n", true), 10).is_err());
    }
}
