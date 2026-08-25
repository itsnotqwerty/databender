use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use databender::codecs::encoded_video::mutate_packet;
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

fn write_h264_mp4(path: &std::path::Path) {
    let status = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=2:duration=1",
            "-codec:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-bf",
            "0",
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn write_two_video_mp4(path: &std::path::Path) {
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
            "-y",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn frame_hashes(path: &std::path::Path, stream_index: usize) -> Vec<u8> {
    let output = ProcessCommand::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-i",
            path.to_str().unwrap(),
            "-map",
            &format!("0:v:{stream_index}"),
            "-f",
            "framemd5",
            "-",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

fn video_metadata(path: &std::path::Path) -> Vec<u8> {
    let output = ProcessCommand::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream_tags=title,language",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    output.stdout
}

#[test]
fn probes_codec_aware_video_packets() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    write_multistream_mp4(&input);

    let stream = ToolRunner::default()
        .probe_video_packets(&input, 0)
        .unwrap();

    assert_eq!(stream.stream_index, 0);
    assert_eq!(stream.codec.codec_name, "mpeg4");
    assert!(!stream.codec.time_base.is_empty());
    assert_eq!(stream.packets.len(), 6);
    assert!(stream.packets[0].keyframe);
    assert!(stream
        .packets
        .iter()
        .all(|packet| packet.size > 0 && packet.duration.is_some()));
    assert!(stream
        .packets
        .windows(2)
        .all(|packets| packets[0].pts <= packets[1].pts));
}

#[test]
fn reads_and_adapts_real_h264_packet_payloads() {
    let runner = ToolRunner::default();
    if !ffmpeg_available() || !runner.supports_encoder("libx264") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    write_h264_mp4(&input);
    let stream = runner.probe_video_packets(&input, 0).unwrap();
    let first = &stream.packets[0];
    let original = runner.read_video_packet(&input, first).unwrap();
    let mut mutated = original.clone();

    let impact = mutate_packet(
        &stream.codec.codec_name,
        &mut mutated,
        stream.codec.nal_length_size,
        8,
        0.5,
        42,
    )
    .unwrap();

    assert_eq!(stream.codec.nal_length_size, Some(4));
    assert!(impact.eligible_bytes >= impact.mutated_bytes);
    assert!(impact.mutated_bytes > 0);
    assert_ne!(mutated, original);
    assert_eq!(runner.read_video_packet(&input, first).unwrap(), original);
}

#[test]
fn mutates_mp4_packets_deterministically_without_rewriting_the_container() {
    let runner = ToolRunner::default();
    if !ffmpeg_available() || !runner.supports_encoder("libx264") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let first_output = directory.path().join("first.mp4");
    let second_output = directory.path().join("second.mp4");
    write_h264_mp4(&input);
    let stream = runner.probe_video_packets(&input, 0).unwrap();

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

    let source = std::fs::read(&input).unwrap();
    let first = std::fs::read(&first_output).unwrap();
    let second = std::fs::read(&second_output).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), source.len());
    assert_ne!(first, source);
    let changed = source
        .iter()
        .zip(&first)
        .enumerate()
        .filter_map(|(index, (before, after))| (before != after).then_some(index as u64))
        .collect::<Vec<_>>();
    assert!(!changed.is_empty());
    assert!(changed
        .iter()
        .all(|offset| stream.packets.iter().any(|packet| {
            let start = packet.position.unwrap();
            start <= *offset && *offset < start + packet.size as u64
        })));
}

#[test]
fn reports_video_packet_mutation_impact() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output_directory = directory.path().join("output");
    write_h264_mp4(&input);
    let filter = "video-packet-noise:byte_budget=12,start_packet=3,packet_count=4,frame_type=delta,intensity=0.25,max_frame_loss=2";
    let estimate = "up to 12 protected-payload bytes per delta video packets 3 through 6, flipping up to 2 bits per byte; tolerate 2 lost frames";

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "plan", "--format", "mp4", "--seed", "42", "--filter", filter,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "impact estimate: {estimate}"
        )));

    let output = Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output_directory.to_str().unwrap(),
            "--dry-run",
            "--json",
            "--seed",
            "42",
            "--filter",
            filter,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["mutation_impacts"][0]["filter"],
        "video-packet-noise"
    );
    assert_eq!(report["mutation_impacts"][0]["estimate"], estimate);
    assert!(!output_directory.exists());
}

#[test]
fn transforms_selected_mp4_video_and_copies_unselected_video() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output = directory.path().join("output.mp4");
    write_two_video_mp4(&input);

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
    assert_eq!(streams[0].properties.width, 160);
    assert_eq!(streams[0].properties.height, 90);
    assert_eq!(streams[1].properties.width, 96);
    assert_eq!(streams[1].properties.height, 64);
    assert_eq!(streams[0].frame_count, 3);
    assert_eq!(streams[0].frame_rate.as_deref(), Some("10/1"));
    assert_eq!(streams[1].frame_count, 2);
    assert_eq!(streams[1].frame_rate.as_deref(), Some("5/1"));
    assert_eq!(streams[0].frame_count, input_streams[0].frame_count);
    assert_eq!(streams[0].frame_rate, input_streams[0].frame_rate);
    assert_eq!(streams[1].frame_count, input_streams[1].frame_count);
    assert_eq!(streams[1].frame_rate, input_streams[1].frame_rate);
    assert_eq!(frame_hashes(&input, 0), frame_hashes(&output, 0));
    assert_ne!(frame_hashes(&input, 1), frame_hashes(&output, 1));
    assert_eq!(video_metadata(&input), video_metadata(&output));
}

#[test]
fn transforms_multiple_selected_mp4_video_streams() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let output = directory.path().join("output.mp4");
    write_two_video_mp4(&input);

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
            "0",
            "--video-stream",
            "1",
        ])
        .assert()
        .success();

    assert_ne!(frame_hashes(&input, 0), frame_hashes(&output, 0));
    assert_ne!(frame_hashes(&input, 1), frame_hashes(&output, 1));
}

#[test]
fn rejects_invalid_mp4_video_stream_selectors() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    write_two_video_mp4(&input);

    let mut out_of_range = Command::cargo_bin("databender").unwrap();
    out_of_range
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            directory.path().join("range.mp4").to_str().unwrap(),
            "--filter",
            "invert",
            "--video-stream",
            "2",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("out of range"));

    let mut duplicate = Command::cargo_bin("databender").unwrap();
    duplicate
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            directory.path().join("duplicate.mp4").to_str().unwrap(),
            "--filter",
            "invert",
            "--video-stream",
            "1",
            "--video-stream",
            "1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("selected more than once"));
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
