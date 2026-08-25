use std::ffi::OsString;

use crate::{
    ffmpeg::ToolRunner,
    filters::{audio::compile_audio_graph, FilterDomain},
    DatabenderError, PreparedTransform, Result,
};

pub fn execute(prepared: PreparedTransform) -> Result<std::path::PathBuf> {
    let runner = ToolRunner::default();
    let expected = runner.probe_audio(prepared.input())?.properties;
    let filters = prepared
        .plan()
        .stages
        .iter()
        .flat_map(|stage| {
            assert_eq!(stage.domain, FilterDomain::FfmpegAudio);
            stage.filters.iter().cloned()
        })
        .collect::<Vec<_>>();
    let graph = compile_audio_graph(&filters)?;
    let input = prepared.input().to_path_buf();
    let encoder = runner.clone();

    prepared.publish_path_with(
        move |candidate| {
            encoder.ffmpeg([
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
                candidate.as_os_str().to_owned(),
            ])?;
            Ok(())
        },
        move |candidate| validate(candidate, expected, &runner),
    )
}

fn validate(
    candidate: &std::path::Path,
    expected: crate::ffmpeg::AudioProperties,
    runner: &ToolRunner,
) -> Result<()> {
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
