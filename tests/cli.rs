use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

#[test]
fn lists_formats_with_honest_statuses() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .arg("list-formats")
        .assert()
        .success()
        .stdout(predicate::str::contains("jpeg\tpixel + Huffman filters"))
        .stdout(predicate::str::contains("png\tpixel + payload filters"))
        .stdout(predicate::str::contains("wav\tPCM + payload filters"))
        .stdout(predicate::str::contains(
            "mp3\tencoded main-data + FFmpeg audio filters",
        ))
        .stdout(predicate::str::contains(
            "mp4\tpacket + pixel + PCM + FFmpeg filters",
        ))
        .stdout(predicate::str::contains(
            "mkv\tpacket + pixel + PCM + FFmpeg filters",
        ))
        .stdout(predicate::str::contains("webp\tpixel filters"))
        .stdout(predicate::str::contains("avif\tpixel filters"))
        .stdout(predicate::str::contains(
            "ogg\tencoded packet + PCM + FFmpeg audio filters",
        ));
}

#[test]
fn lists_filters_by_codec_and_domain() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .arg("list-filters")
        .assert()
        .success()
        .stdout(predicate::str::contains("jpeg [image pixels]"))
        .stdout(predicate::str::contains("  hue-rotate"))
        .stdout(predicate::str::contains("jpeg [Huffman tables]"))
        .stdout(predicate::str::contains("  huffman-glitch"))
        .stdout(predicate::str::contains("png [encoded payload]"))
        .stdout(predicate::str::contains("  byte-swap"))
        .stdout(predicate::str::contains("webp [image pixels]"))
        .stdout(predicate::str::contains("avif [image pixels]"))
        .stdout(predicate::str::contains("wav [PCM audio]"))
        .stdout(predicate::str::contains("wav [encoded payload]"))
        .stdout(predicate::str::contains("mp3 [encoded main data]"))
        .stdout(predicate::str::contains("  mp3-main-data-noise"))
        .stdout(predicate::str::contains("mp3 [FFmpeg audio]"))
        .stdout(predicate::str::contains("ogg [encoded packets]"))
        .stdout(predicate::str::contains("  ogg-packet-noise"))
        .stdout(predicate::str::contains("ogg [PCM audio]"))
        .stdout(predicate::str::contains("ogg [FFmpeg audio]"))
        .stdout(predicate::str::contains("  high-pass"))
        .stdout(predicate::str::contains("mp4 [encoded video packets]"))
        .stdout(predicate::str::contains("  video-packet-noise"))
        .stdout(predicate::str::contains("mp4 [image pixels]"))
        .stdout(predicate::str::contains("mp4 [PCM audio]"))
        .stdout(predicate::str::contains("mp4 [FFmpeg audio]"))
        .stdout(predicate::str::contains("mp4 [FFmpeg video]"))
        .stdout(predicate::str::contains("mkv [image pixels]"))
        .stdout(predicate::str::contains("mkv [PCM audio]"))
        .stdout(predicate::str::contains("mkv [encoded video packets]"));
}

#[test]
fn lists_discovered_plugins_with_state_compatibility_and_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "abi_version": 1,
        "id": "example.frames",
        "name": "Example Frames",
        "version": "1.2.3",
        "filters": [{
            "id": "invert",
            "name": "Plugin Invert",
            "description": "Inverts frame pixels",
            "domains": ["image-frame"],
            "deterministic": true,
            "parameters": []
        }]
    });
    let manifest_path = directory.path().join("frames.plugin.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    fs::write(
        directory.path().join("frames.wasm"),
        br#"(module
            (memory (export "memory") 1)
            (func (export "databender_alloc") (param i32) (result i32) i32.const 0)
            (func (export "databender_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .unwrap();
    let missing = directory.path().join("missing.plugin.json");
    let mut missing_manifest = manifest;
    missing_manifest["id"] = "example.missing".into();
    fs::write(&missing, serde_json::to_vec(&missing_manifest).unwrap()).unwrap();
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "list-plugins",
            "--plugin-dir",
            directory.path().to_str().unwrap(),
            "--disable-plugin",
            "example.frames",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("example.frames 1.2.3"))
        .stdout(predicate::str::contains("disabled\tcompatible"))
        .stdout(predicate::str::contains(
            "invert\tPlugin Invert\tImageFrame",
        ))
        .stdout(predicate::str::contains(manifest_path.to_str().unwrap()))
        .stdout(predicate::str::contains("example.missing 1.2.3"))
        .stdout(predicate::str::contains(
            "incompatible: could not read module",
        ));
}

#[test]
fn plans_ordered_compatible_filters() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "plan",
            "--format",
            "png",
            "--seed",
            "42",
            "--filter",
            "channel-shift",
            "--filter",
            "byte-noise",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("plan-version: 1"))
        .stdout(predicate::str::contains("stage 1: Image/ImagePixels"))
        .stdout(predicate::str::contains("stage 2: Image/EncodedPayload"));
}

#[test]
fn rejects_incompatible_filters_before_execution() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "plan",
            "--format",
            "mp4",
            "--seed",
            "42",
            "--filter",
            "byte-noise",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "filter byte-noise is incompatible with mp4",
        ));
}

#[test]
fn accepts_typed_filter_parameters() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "plan",
            "--format",
            "png",
            "--seed",
            "42",
            "--filter",
            "channel-shift:pixels=-12",
            "--filter",
            "pixel-sort:threshold=200",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "filters=channel-shift, pixel-sort",
        ));
}

#[test]
fn rejects_invalid_filter_parameters() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "plan",
            "--format",
            "png",
            "--seed",
            "42",
            "--filter",
            "byte-noise:probability=2",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "invalid value for byte-noise.probability",
        ));
}

#[test]
fn rejects_expert_graph_external_resource_access() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .args([
            "plan",
            "--format",
            "mp4",
            "--seed",
            "42",
            "--filter",
            "expert-video-graph:movie=file:/tmp/input.mp4",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "external protocols are not permitted",
        ));
}

#[test]
fn reports_configured_ffmpeg_path_failure_during_preflight() {
    let mut command = Command::cargo_bin("databender").unwrap();

    command
        .env("DATABENDER_FFMPEG", "/missing/databender-ffmpeg")
        .args([
            "plan",
            "--format",
            "mp3",
            "--seed",
            "42",
            "--filter",
            "volume:gain=1.2",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "configured FFmpeg executable is unavailable",
        ));
}
