use std::{ffi::OsString, path::PathBuf};

use crate::{
    codecs::mp4::process_audio_stages,
    ffmpeg::{AudioProperties, AudioStreamInfo, BasicMetadata, ToolRunner},
    DatabenderError, MediaFormat, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<PathBuf> {
    let runner = ToolRunner::default();
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
    let workspace = tempfile::tempdir().map_err(|source| DatabenderError::Io {
        path: prepared.output().to_path_buf(),
        source,
    })?;
    let processed = streams
        .iter()
        .enumerate()
        .map(|(index, _)| {
            process_audio_stages(
                &runner,
                &input,
                index,
                &prepared.plan().stages,
                workspace.path(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let encoder = runner.clone();
    let expected_streams = streams.clone();

    prepared.publish_path_with(
        move |candidate| {
            encoder.ffmpeg(encode_arguments(&input, candidate, &processed, &streams)?)?;
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
            )
        },
    )
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
) -> Result<()> {
    if MediaFormat::detect(candidate)? != MediaFormat::Ogg {
        return Err(invalid_ogg("candidate container changed"));
    }
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
    runner.ffmpeg([
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
    Ok(())
}

fn invalid_ogg(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid Ogg: {}", reason.into()),
    }
}
