use std::fs;

use assert_cmd::Command;
use image::{GenericImageView, ImageBuffer, Rgb, Rgba};
use predicates::prelude::*;

fn write_png(path: &std::path::Path) {
    let image = ImageBuffer::from_fn(4, 2, |x, y| {
        Rgba([(x * 50) as u8, (y * 100) as u8, (x * 20) as u8, 255_u8])
    });
    image.save(path).unwrap();
}

fn write_jpeg(path: &std::path::Path) {
    let image = ImageBuffer::from_fn(4, 2, |x, y| {
        Rgb([(x * 50) as u8, (y * 100) as u8, (x * 20) as u8])
    });
    image.save(path).unwrap();
}

fn write_textured_jpeg(path: &std::path::Path) {
    let image = ImageBuffer::from_fn(64, 64, |x, y| {
        Rgb([
            ((x * 17 + y * 29) % 256) as u8,
            ((x * x + y * 11) % 256) as u8,
            ((x * 7 + y * y) % 256) as u8,
        ])
    });
    image.save(path).unwrap();
}

#[test]
fn transforms_png_with_ordered_pixel_filters() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.png");
    write_png(&input);

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
            "channel-shift:pixels=1",
            "--filter",
            "scanline-displacement:max_shift=2",
            "--filter",
            "pixel-sort:threshold=40",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

    assert!(output.exists());
    assert_eq!(image::open(output).unwrap().width(), 4);
}

#[test]
fn generated_seed_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.png");
    write_png(&input);

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--filter",
            "channel-shift:pixels=1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: "));
}

#[test]
fn transforms_png_with_ordered_payload_filters() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.png");
    write_png(&input);

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
            "byte-noise:probability=0.2",
            "--filter",
            "byte-repeat:count=2",
            "--filter",
            "byte-drop:count=2",
            "--filter",
            "byte-swap:count=2",
        ])
        .assert()
        .success();

    assert_eq!(image::open(output).unwrap().dimensions(), (4, 2));
    assert!(fs::metadata(input).is_ok());
}

#[test]
fn jpeg_payload_filter_fails_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.jpg");
    let output = directory.path().join("output.jpg");
    write_jpeg(&input);

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
            "byte-noise",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the EncodedPayload domain is not supported",
        ));

    assert!(!output.exists());
    assert!(input.exists());
}

#[test]
fn transforms_jpeg_with_new_pixel_filters() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.jpg");
    let output = directory.path().join("output.jpg");
    write_jpeg(&input);

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
            "brightness:delta=20",
            "--filter",
            "contrast:factor=1.2",
            "--filter",
            "saturation:factor=1.4",
            "--filter",
            "hue-rotate:degrees=60",
            "--filter",
            "posterize:bits=4",
            "--filter",
            "invert",
            "--filter",
            "row-dropout:probability=0.2",
        ])
        .assert()
        .success();

    assert_eq!(image::open(output).unwrap().dimensions(), (4, 2));
}

#[test]
fn huffman_glitch_radically_changes_jpeg_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.jpg");
    let output = directory.path().join("output.jpg");
    write_textured_jpeg(&input);

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
            "huffman-glitch:swaps=128,intensity=1,target=all,mode=run-remap,preserve_size=true",
        ])
        .assert()
        .success();

    let before = image::open(&input).unwrap().into_rgb8();
    let after = image::open(&output).unwrap().into_rgb8();
    let changed_channels = before
        .as_raw()
        .iter()
        .zip(after.as_raw())
        .filter(|(left, right)| left.abs_diff(**right) > 16)
        .count();

    assert_eq!(after.dimensions(), before.dimensions());
    assert_ne!(fs::read(input).unwrap(), fs::read(output).unwrap());
    assert!(changed_channels > before.as_raw().len() / 4);
}

#[test]
fn overwrites_output_by_default() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.png");
    write_png(&input);
    fs::write(&output, b"old output").unwrap();

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

    assert!(image::open(output).is_ok());
}

#[test]
fn protect_output_refuses_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.png");
    write_png(&input);
    fs::write(&output, b"old output").unwrap();

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
            "--protect-output",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to overwrite existing output",
        ));

    assert_eq!(fs::read(output).unwrap(), b"old output");
}
