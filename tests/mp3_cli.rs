use std::{fs, process::Command as ProcessCommand};

use assert_cmd::Command;
use databender::codecs::mp3_frames;
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
fn parses_real_mp3_main_data_without_overlapping_structure() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    write_mp3(&input);
    let encoded = fs::read(&input).unwrap();

    let structure = mp3_frames::parse(&encoded).unwrap();

    assert!(!structure.frames.is_empty());
    for frame in &structure.frames {
        assert_eq!(
            frame.header.end,
            frame
                .crc
                .as_ref()
                .map_or(frame.side_information.start, |crc| crc.start)
        );
        assert!(frame
            .crc
            .as_ref()
            .is_none_or(|crc| crc.end == frame.side_information.start));
        assert_eq!(frame.side_information.end, frame.main_data.start);
        assert_eq!(frame.main_data.end, frame.frame.end);
        assert!(structure
            .leading_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.end <= frame.frame.start));
        assert!(structure
            .trailing_metadata
            .as_ref()
            .is_none_or(|metadata| frame.frame.end <= metadata.start));
    }
}

#[test]
fn mutates_mp3_main_data_deterministically_and_fully_decodes() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let first = directory.path().join("first.mp3");
    let second = directory.path().join("second.mp3");
    write_mp3(&input);
    let filter = "mp3-main-data-noise:byte_budget=1,start_frame=2,frame_count=2,intensity=0.125";

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

    let input_bytes = fs::read(&input).unwrap();
    let first_bytes = fs::read(&first).unwrap();
    assert_ne!(first_bytes, input_bytes);
    assert_eq!(first_bytes, fs::read(&second).unwrap());
    let original = mp3_frames::parse(&input_bytes).unwrap();
    let changed = input_bytes
        .iter()
        .zip(&first_bytes)
        .enumerate()
        .filter_map(|(index, (before, after))| (before != after).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(changed.len(), 1);
    assert!(original.frames[2..4]
        .iter()
        .any(|frame| frame.main_data.contains(&changed[0])));
    mp3_frames::parse(&first_bytes).unwrap();
    ToolRunner::default()
        .validate_audio(
            &first,
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap();
}

#[test]
fn reports_mp3_mutation_impact_in_plans_and_batch_dry_runs() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output_directory = directory.path().join("output");
    write_mp3(&input);
    let filter = "mp3-main-data-noise:byte_budget=12,start_frame=3,frame_count=4,intensity=0.25";
    let estimate = "up to 12 main-data bytes in frames 3 through 6, flipping up to 2 bits per byte";

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "plan", "--format", "mp3", "--seed", "42", "--filter", filter,
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
        "mp3-main-data-noise"
    );
    assert_eq!(report["mutation_impacts"][0]["estimate"], estimate);
    assert!(!output_directory.exists());
}

#[test]
fn preserves_order_between_encoded_and_ffmpeg_mp3_stages() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output = directory.path().join("output.mp3");
    write_mp3(&input);

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
            "mp3-main-data-noise:byte_budget=1,start_frame=2,intensity=0.125",
            "--filter",
            "volume:gain=0.8",
        ])
        .assert()
        .success();

    mp3_frames::parse(&fs::read(&output).unwrap()).unwrap();
    ToolRunner::default()
        .validate_audio(
            &output,
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap();
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

#[test]
fn executes_explicit_expert_audio_graph() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output = directory.path().join("output.mp3");
    write_mp3(&input);

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
            "expert-audio-graph:volume=0.5,aecho=0.8:0.9:20:0.2",
        ])
        .assert()
        .success();

    ToolRunner::default()
        .validate_audio(
            &output,
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap();
}

#[test]
fn rejects_unavailable_expert_filter_before_publication() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output = directory.path().join("output.mp3");
    write_mp3(&input);

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
            "expert-audio-graph:databender_filter_that_does_not_exist=1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "expert graph requires unavailable FFmpeg filter",
        ));

    assert!(!output.exists());
}

#[test]
fn reports_resolved_expert_graphs_in_plans_and_batch_manifests() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp3");
    let output_directory = directory.path().join("output");
    write_mp3(&input);
    let graph = "volume=0.5,aecho=0.8:0.9:20:0.2";

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "plan",
            "--format",
            "mp3",
            "--seed",
            "42",
            "--filter",
            &format!("expert-audio-graph:{graph}"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("environment-dependent: true"))
        .stdout(predicate::str::contains(format!("resolved graph: {graph}")));

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
            &format!("expert-audio-graph:{graph}"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["environment_dependent"], true);
    assert_eq!(report["resolved_graphs"][0]["target"], "audio");
    assert_eq!(report["resolved_graphs"][0]["graph"], graph);
}
