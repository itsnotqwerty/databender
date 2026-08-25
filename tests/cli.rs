use assert_cmd::Command;
use predicates::prelude::*;

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
        .stdout(predicate::str::contains("mp3\tFFmpeg audio filters"))
        .stdout(predicate::str::contains(
            "mp4\tpixel + PCM + FFmpeg filters",
        ))
        .stdout(predicate::str::contains(
            "mkv\tpixel + PCM + FFmpeg filters",
        ))
        .stdout(predicate::str::contains("webp\tpixel filters"))
        .stdout(predicate::str::contains("avif\tpixel filters"))
        .stdout(predicate::str::contains("ogg\tPCM + FFmpeg audio filters"));
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
        .stdout(predicate::str::contains("mp3 [FFmpeg audio]"))
        .stdout(predicate::str::contains("ogg [PCM audio]"))
        .stdout(predicate::str::contains("ogg [FFmpeg audio]"))
        .stdout(predicate::str::contains("  high-pass"))
        .stdout(predicate::str::contains("mp4 [image pixels]"))
        .stdout(predicate::str::contains("mp4 [PCM audio]"))
        .stdout(predicate::str::contains("mp4 [FFmpeg audio]"))
        .stdout(predicate::str::contains("mp4 [FFmpeg video]"))
        .stdout(predicate::str::contains("mkv [image pixels]"))
        .stdout(predicate::str::contains("mkv [PCM audio]"));
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
