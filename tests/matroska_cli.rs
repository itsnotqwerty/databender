use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use databender::{
    ffmpeg::{AudioProperties, ToolRunner, VideoProperties},
    MediaFormat,
};
use predicates::prelude::*;

fn ffmpeg_available() -> bool {
    ProcessCommand::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_matroska(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10:duration=0.4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=0.4",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-codec:v",
            "ffv1",
            "-codec:a",
            "flac",
            "-ac:a:0",
            "1",
            "-ac:a:1",
            "2",
            "-metadata",
            "title=Matroska Fixture",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=spa",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn transforms_matroska_video_and_all_audio_streams() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mkv");
    let output = directory.path().join("output.mkv");
    write_matroska(&input);

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
            "invert",
            "--filter",
            "hue:degrees=30",
            "--filter",
            "audio-noise:probability=1,amplitude=0.05",
            "--filter",
            "high-pass:frequency=200",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

    assert_eq!(MediaFormat::detect(&output).unwrap(), MediaFormat::Matroska);
    let runner = ToolRunner::default();
    let video = runner
        .validate_video(
            &output,
            VideoProperties {
                width: 160,
                height: 90,
            },
        )
        .unwrap();
    let audio = runner
        .validate_audio_streams(
            &output,
            &[
                AudioProperties {
                    sample_rate: 48_000,
                    channels: 1,
                },
                AudioProperties {
                    sample_rate: 48_000,
                    channels: 2,
                },
            ],
        )
        .unwrap();
    assert_eq!(video.codec_name, "ffv1");
    assert!(audio.iter().all(|stream| stream.codec_name == "flac"));
    assert_eq!(
        runner.probe_basic_metadata(&output).unwrap(),
        runner.probe_basic_metadata(&input).unwrap()
    );
}
