use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use databender::ffmpeg::{AudioProperties, ToolRunner, VideoProperties};
use predicates::prelude::*;

fn ffmpeg_available() -> bool {
    ProcessCommand::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_multistream_mp4(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10:duration=0.6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=48000:duration=0.6",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-codec:v",
            "mpeg4",
            "-q:v",
            "5",
            "-codec:a",
            "aac",
            "-ac:a:0",
            "1",
            "-ac:a:1",
            "2",
            "-metadata",
            "title=Databender Fixture",
            "-metadata",
            "artist=Databender Tests",
            "-metadata:s:v:0",
            "title=Primary Picture",
            "-metadata:s:v:0",
            "language=jpn",
            "-metadata:s:a:0",
            "title=Main Audio",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "title=Alternate Audio",
            "-metadata:s:a:1",
            "language=spa",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn write_video_only_mp4(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10:duration=0.3",
            "-codec:v",
            "mpeg4",
            "-q:v",
            "5",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn transforms_one_video_and_all_mp4_audio_streams() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output = directory.path().join("output.mp4");
    write_multistream_mp4(&input);

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
            "hue:degrees=60",
            "--filter",
            "brightness:delta=12",
            "--filter",
            "audio-noise:probability=1,amplitude=0.1",
            "--filter",
            "high-pass:frequency=200",
            "--filter",
            "volume:gain=1.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

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
    assert_eq!(video.codec_name, "mpeg4");
    assert_eq!(audio.len(), 2);
    assert!(audio.iter().all(|stream| stream.codec_name == "aac"));
    let input_metadata = runner.probe_basic_metadata(&input).unwrap();
    let output_metadata = runner.probe_basic_metadata(&output).unwrap();
    assert_eq!(output_metadata, input_metadata);
    assert_eq!(
        output_metadata.format.get("title").map(String::as_str),
        Some("Databender Fixture")
    );
    assert_eq!(
        output_metadata.audio[0].get("language").map(String::as_str),
        Some("eng")
    );
    assert_eq!(
        output_metadata.audio[1].get("language").map(String::as_str),
        Some("spa")
    );

    let streams = ProcessCommand::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(streams.status.success());
    let stream_types = String::from_utf8_lossy(&streams.stdout);
    assert_eq!(
        stream_types.lines().filter(|line| *line == "video").count(),
        1
    );
    assert_eq!(
        stream_types.lines().filter(|line| *line == "audio").count(),
        2
    );
}

#[test]
fn transforms_video_only_mp4_without_requiring_audio() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output = directory.path().join("output.mp4");
    write_video_only_mp4(&input);

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
            "hue:degrees=60",
        ])
        .assert()
        .success();

    let runner = ToolRunner::default();
    runner
        .validate_video(
            &output,
            VideoProperties {
                width: 160,
                height: 90,
            },
        )
        .unwrap();
    assert!(runner.probe_audio_streams(&output).unwrap().is_empty());
}

#[test]
fn video_filters_copy_all_unfiltered_audio_streams() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output = directory.path().join("output.mp4");
    write_multistream_mp4(&input);

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
        ])
        .assert()
        .success();

    let audio = ToolRunner::default().probe_audio_streams(&output).unwrap();
    assert_eq!(audio.len(), 2);
    assert_eq!(audio[0].properties.channels, 1);
    assert_eq!(audio[1].properties.channels, 2);
}

#[test]
fn plans_decoded_pixel_and_pcm_filters_for_mp4() {
    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "plan",
            "--format",
            "mp4",
            "--seed",
            "42",
            "--filter",
            "channel-shift",
            "--filter",
            "audio-noise",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Image/ImagePixels"))
        .stdout(predicate::str::contains("Audio/PcmAudio"));
}
