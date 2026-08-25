use std::{fs, process::Command as ProcessCommand};

use assert_cmd::Command;
use databender::ffmpeg::{AudioProperties, ToolRunner};
use predicates::prelude::*;

fn ffmpeg_available() -> bool {
    ProcessCommand::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_mp3(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=0.2",
            "-ac",
            "2",
            "-metadata",
            "title=Databender Fixture",
            "-codec:a",
            "libmp3lame",
            "-q:a",
            "2",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn transforms_mp3_with_ordered_audio_effects() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output = directory.path().join("output.mp3");
    write_mp3(&input);

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
            "high-pass:frequency=200",
            "--filter",
            "low-pass:frequency=3000",
            "--filter",
            "volume:gain=1.5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

    assert_ne!(fs::read(&input).unwrap(), fs::read(&output).unwrap());
    let stream = ToolRunner::default()
        .validate_audio(
            &output,
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap();
    assert_eq!(stream.codec_name, "mp3");

    let metadata = ProcessCommand::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=title",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(metadata.status.success());
    assert_eq!(
        String::from_utf8_lossy(&metadata.stdout).trim(),
        "Databender Fixture"
    );
}

#[test]
fn rejects_pcm_noise_for_mp3_during_preflight() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output = directory.path().join("output.mp3");
    write_mp3(&input);

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
            "audio-noise",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "filter audio-noise is incompatible with mp3",
        ));

    assert!(!output.exists());
}
