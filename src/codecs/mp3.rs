use std::{ffi::OsString, fs, path::Path};

use crate::{
    ffmpeg::ToolRunner,
    filters::{audio::compile_audio_graph, mp3 as mutation, FilterDomain},
    DatabenderError, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<std::path::PathBuf> {
    let runner = ToolRunner::default().with_cancellation(prepared.cancellation().clone());
    let expected = runner.probe_audio(prepared.input())?.properties;
    let stages = prepared.plan().stages.clone();
    let input = prepared.input().to_path_buf();
    let encoder = runner.clone();
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
                let output = workspace.path().join(format!("stage-{stage_index}.mp3"));
                match stage.domain {
                    FilterDomain::FfmpegAudio => encode_audio_stage(
                        &current,
                        &output,
                        &compile_audio_graph(&stage.filters)?,
                        &encoder,
                    )?,
                    FilterDomain::Mp3MainData => {
                        let mut encoded = read(&current)?;
                        for (filter_index, filter) in stage.filters.iter().enumerate() {
                            cancellation.check()?;
                            mutation::apply(
                                filter,
                                &mut encoded,
                                stage.seed.wrapping_add(filter_index as u64),
                            )?;
                        }
                        write(&output, &encoded)?;
                    }
                    _ => unreachable!("MP3 plans contain only audio and main-data stages"),
                }
                current = output;
            }
            fs::copy(&current, candidate).map_err(|source| DatabenderError::Io {
                path: candidate.to_path_buf(),
                source,
            })?;
            Ok(())
        },
        move |candidate| validate(candidate, expected, &runner),
    )
}

fn encode_audio_stage(input: &Path, output: &Path, graph: &str, runner: &ToolRunner) -> Result<()> {
    runner
        .ffmpeg([
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            input.as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from("0:a:0"),
            OsString::from("-map_metadata"),
            OsString::from("0"),
            OsString::from("-af"),
            OsString::from(graph),
            OsString::from("-codec:a"),
            OsString::from("libmp3lame"),
            OsString::from("-q:a"),
            OsString::from("2"),
            OsString::from("-f"),
            OsString::from("mp3"),
            output.as_os_str().to_owned(),
        ])
        .map(|_| ())
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write(path: &Path, encoded: &[u8]) -> Result<()> {
    fs::write(path, encoded).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn validate(
    candidate: &std::path::Path,
    expected: crate::ffmpeg::AudioProperties,
    runner: &ToolRunner,
) -> Result<()> {
    let encoded = read(candidate)?;
    crate::codecs::mp3_frames::parse(&encoded)?;
    let stream = runner.validate_audio(candidate, expected)?;
    if stream.codec_name != "mp3" {
        return Err(DatabenderError::OutputValidation {
            reason: format!("FFmpeg encoded {} instead of mp3", stream.codec_name),
        });
    }
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        candidate.as_os_str().to_owned(),
        OsString::from("-map"),
        OsString::from("0:a:0"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ])?;
    Ok(())
}
