use std::{fs, process::Command as ProcessCommand};

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
    let directory = path.parent().unwrap();
    let subtitle = directory.join("captions.srt");
    let attachment = directory.join("fixture.ttf");
    let chapters = directory.join("chapters.ffmeta");
    fs::write(
        &subtitle,
        "1\n00:00:00,000 --> 00:00:00,300\nDatabender caption\n",
    )
    .unwrap();
    fs::write(&attachment, b"fixture attachment").unwrap();
    fs::write(
        &chapters,
        ";FFMETADATA1\ntitle=Matroska Fixture\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=200\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=200\nEND=400\ntitle=Closing\n",
    )
    .unwrap();

    let mut command = ProcessCommand::new("ffmpeg");
    command
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
            "-f",
            "srt",
            "-i",
        ])
        .arg(&subtitle)
        .args(["-f", "ffmetadata", "-i"])
        .arg(&chapters)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-map",
            "2:a:0",
            "-map",
            "3:s:0",
            "-map_metadata",
            "4",
            "-map_chapters",
            "4",
            "-codec:v",
            "ffv1",
            "-codec:a",
            "flac",
            "-codec:s",
            "srt",
            "-ac:a:0",
            "1",
            "-ac:a:1",
            "2",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=spa",
            "-metadata:s:s:0",
            "language=eng",
            "-attach",
        ])
        .arg(&attachment)
        .args([
            "-metadata:s:t:0",
            "filename=fixture.ttf",
            "-metadata:s:t:0",
            "mimetype=application/x-truetype-font",
            "-y",
        ])
        .arg(path);
    let status = command.status().unwrap();
    assert!(status.success());
}

fn write_two_video_matroska(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x90:rate=10:duration=0.3",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=96x64:rate=5:duration=0.4",
            "-map",
            "0:v:0",
            "-map",
            "1:v:0",
            "-codec:v",
            "mpeg4",
            "-q:v",
            "5",
            "-metadata:s:v:0",
            "title=Primary Picture",
            "-metadata:s:v:0",
            "language=eng",
            "-metadata:s:v:1",
            "title=Alternate Picture",
            "-metadata:s:v:1",
            "language=spa",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn write_h264_audio_matroska(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=2:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-codec:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-bf",
            "0",
            "-codec:a",
            "libopus",
            "-metadata",
            "title=Packet Fixture",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn probes_matroska_packets_without_losing_auxiliary_topology() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mkv");
    write_matroska(&input);

    let runner = ToolRunner::default();
    let stream = runner.probe_video_packets(&input, 0).unwrap();
    let auxiliary = runner.probe_matroska_auxiliary(&input).unwrap();

    assert_eq!(stream.codec.codec_name, "ffv1");
    assert_eq!(stream.packets.len(), 4);
    assert!(stream.packets.iter().all(|packet| packet.size > 0));
    assert_eq!(auxiliary.subtitles.len(), 1);
    assert_eq!(auxiliary.attachments.len(), 1);
    assert_eq!(auxiliary.chapters.len(), 2);
}

#[test]
fn mutates_matroska_packets_deterministically_without_rewriting_blocks() {
    let runner = ToolRunner::default();
    if !ffmpeg_available()
        || !runner.supports_encoder("libx264")
        || !runner.supports_encoder("libopus")
    {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mkv");
    let first_output = directory.path().join("first.mkv");
    let second_output = directory.path().join("second.mkv");
    write_h264_audio_matroska(&input);

    for output in [&first_output, &second_output] {
        Command::cargo_bin("databender")
            .unwrap()
            .args([
                "transform",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--seed",
                "42",
                "--filter",
                "video-packet-noise:byte_budget=1,start_packet=1,packet_count=1,frame_type=delta,intensity=0.125,max_frame_loss=1",
            ])
            .assert()
            .success();
    }

    let source = fs::read(&input).unwrap();
    let first = fs::read(&first_output).unwrap();
    assert_eq!(first, fs::read(&second_output).unwrap());
    assert_eq!(first.len(), source.len());
    assert_ne!(first, source);
    assert_eq!(
        runner.probe_basic_metadata(&first_output).unwrap(),
        runner.probe_basic_metadata(&input).unwrap()
    );
    assert_eq!(
        runner.probe_matroska_auxiliary(&first_output).unwrap(),
        runner.probe_matroska_auxiliary(&input).unwrap()
    );
}

#[test]
fn transforms_selected_matroska_video_and_copies_unselected_video() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mkv");
    let output = directory.path().join("output.mkv");
    write_two_video_matroska(&input);

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--filter",
            "invert",
            "--video-stream",
            "1",
        ])
        .assert()
        .success();

    let runner = ToolRunner::default();
    let input_streams = runner.probe_video_streams(&input).unwrap();
    let streams = runner.probe_video_streams(&output).unwrap();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].codec_name, "mpeg4");
    assert_eq!(streams[0].properties.width, 160);
    assert_eq!(streams[1].codec_name, "ffv1");
    assert_eq!(streams[1].properties.width, 96);
    assert_eq!(streams[0].frame_count, 3);
    assert_eq!(streams[0].frame_rate.as_deref(), Some("10/1"));
    assert_eq!(streams[1].frame_count, 2);
    assert_eq!(streams[1].frame_rate.as_deref(), Some("5/1"));
    assert_eq!(streams[0].frame_count, input_streams[0].frame_count);
    assert_eq!(streams[1].frame_count, input_streams[1].frame_count);
    assert_eq!(
        runner.probe_basic_metadata(&output).unwrap(),
        runner.probe_basic_metadata(&input).unwrap()
    );
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
    let auxiliary = runner.probe_matroska_auxiliary(&output).unwrap();
    assert_eq!(auxiliary, runner.probe_matroska_auxiliary(&input).unwrap());
    assert_eq!(auxiliary.subtitles.len(), 1);
    assert_eq!(auxiliary.attachments.len(), 1);
    assert_eq!(auxiliary.chapters.len(), 2);
}
