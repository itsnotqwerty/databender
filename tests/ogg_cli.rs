use std::{fs, process::Command as ProcessCommand};

use assert_cmd::Command;
use databender::{codecs::ogg_pages, ffmpeg::ToolRunner, MediaFormat};
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

fn mutate_codec(codec: &str) {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join(format!("input-{codec}.ogg"));
    let first = directory.path().join(format!("first-{codec}.ogg"));
    let second = directory.path().join(format!("second-{codec}.ogg"));
    write_ogg(&input, codec);
    let filter = "ogg-packet-noise:byte_budget=1,start_packet=1,packet_count=2,intensity=0.125";

    for output in [&first, &second] {
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
                filter,
            ])
            .assert()
            .success();
    }

    assert_ne!(fs::read(&input).unwrap(), fs::read(&first).unwrap());
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    ogg_pages::parse(&fs::read(&first).unwrap()).unwrap();
    let runner = ToolRunner::default();
    let expected = runner.probe_audio(&input).unwrap();
    let actual = runner.validate_audio(&first, expected.properties).unwrap();
    assert_eq!(actual.codec_name, codec);
}

#[test]
fn mutates_vorbis_packets_deterministically_and_fully_decodes() {
    mutate_codec("vorbis");
}

#[test]
fn mutates_opus_packets_deterministically_and_fully_decodes() {
    mutate_codec("opus");
}

#[test]
fn preserves_order_between_packet_and_ffmpeg_ogg_stages() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.ogg");
    let output = directory.path().join("output.ogg");
    write_ogg(&input, "opus");

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
            "ogg-packet-noise:byte_budget=1,start_packet=1,packet_count=2,intensity=0.125",
            "--filter",
            "volume:gain=0.9",
        ])
        .assert()
        .success();

    ogg_pages::parse(&fs::read(&output).unwrap()).unwrap();
    let runner = ToolRunner::default();
    let expected = runner.probe_audio(&input).unwrap();
    assert_eq!(
        runner
            .validate_audio(&output, expected.properties)
            .unwrap()
            .codec_name,
        "opus"
    );
}

#[test]
fn reports_ogg_packet_mutation_impact() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.ogg");
    let output_directory = directory.path().join("output");
    write_ogg(&input, "opus");
    let filter = "ogg-packet-noise:byte_budget=12,start_packet=3,packet_count=4,intensity=0.25,max_decode_errors=2";
    let estimate = "up to 12 payload bytes in audio packets 3 through 6, flipping up to 2 bits per byte; tolerate 2 decoder error lines";

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "plan", "--format", "ogg", "--seed", "42", "--filter", filter,
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
    assert_eq!(report["mutation_impacts"][0]["filter"], "ogg-packet-noise");
    assert_eq!(report["mutation_impacts"][0]["estimate"], estimate);
    assert!(!output_directory.exists());
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
