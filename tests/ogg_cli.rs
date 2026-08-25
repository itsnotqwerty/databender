use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use databender::{ffmpeg::ToolRunner, MediaFormat};
use predicates::prelude::*;

fn ffmpeg_available() -> bool {
    ProcessCommand::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_ogg(path: &std::path::Path, codec: &str) {
    let encoder = match codec {
        "vorbis" => "libvorbis",
        "opus" => "libopus",
        _ => unreachable!(),
    };
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.3",
            "-ac",
            "2",
            "-codec:a",
            encoder,
            "-metadata",
            "title=Ogg Fixture",
            "-metadata",
            "artist=Databender Tests",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn transform_codec(codec: &str) {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join(format!("input-{codec}.ogg"));
    let output = directory.path().join(format!("output-{codec}.ogg"));
    write_ogg(&input, codec);

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "audio-noise:probability=1,amplitude=0.05",
            "--filter",
            "high-pass:frequency=200",
            "--filter",
            "volume:gain=1.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

    assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Ogg);
    let runner = ToolRunner::default();
    let expected = runner.probe_audio(&input).unwrap();
    let actual = runner.validate_audio(&output, expected.properties).unwrap();
    assert_eq!(actual.codec_name, codec);
    assert_eq!(
        runner.probe_basic_metadata(&output).unwrap(),
        runner.probe_basic_metadata(&input).unwrap()
    );
}

#[test]
fn transforms_ogg_vorbis() {
    transform_codec("vorbis");
}

#[test]
fn transforms_ogg_opus() {
    transform_codec("opus");
}

#[test]
fn rejects_unsupported_ogg_codec_without_publishing() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input-flac.ogg");
    let output = directory.path().join("output.ogg");
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.1",
            "-codec:a",
            "flac",
            "-f",
            "ogg",
            "-y",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--filter",
            "volume:gain=1.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unsupported Ogg audio codec flac; expected Vorbis or Opus",
        ));
    assert!(!output.exists());
}
