use std::{fs, process::Command as ProcessCommand};

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

fn write_transparent_image(path: &std::path::Path) {
    let image = ImageBuffer::from_fn(4, 2, |x, y| {
        Rgba([
            (x * 50) as u8,
            (y * 100) as u8,
            (x * 20) as u8,
            (64 + x * 40 + y * 16) as u8,
        ])
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

fn run_ffmpeg(arguments: &[&std::ffi::OsStr]) {
    let status = ProcessCommand::new(
        std::env::var_os("DATABENDER_FFMPEG").unwrap_or_else(|| "ffmpeg".into()),
    )
    .args(arguments)
    .status()
    .unwrap();
    assert!(status.success());
}

fn avifenc_available() -> bool {
    ProcessCommand::new(std::env::var_os("DATABENDER_AVIFENC").unwrap_or_else(|| "avifenc".into()))
        .arg("--version")
        .status()
        .is_ok_and(|status| status.success())
}

fn run_avifenc(arguments: &[&std::ffi::OsStr]) {
    let status = ProcessCommand::new(
        std::env::var_os("DATABENDER_AVIFENC").unwrap_or_else(|| "avifenc".into()),
    )
    .args(arguments)
    .status()
    .unwrap();
    assert!(status.success());
}

fn webp_animation_timing(path: &std::path::Path) -> (u16, Vec<u32>) {
    let encoded = fs::read(path).unwrap();
    let mut offset = 12;
    let mut loop_count = None;
    let mut durations = Vec::new();
    while offset < encoded.len() {
        let kind = &encoded[offset..offset + 4];
        let size = u32::from_le_bytes(encoded[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let data = &encoded[offset + 8..offset + 8 + size];
        if kind == b"ANIM" {
            loop_count = Some(u16::from_le_bytes(data[4..6].try_into().unwrap()));
        } else if kind == b"ANMF" {
            durations.push(
                u32::from(data[12]) | (u32::from(data[13]) << 8) | (u32::from(data[14]) << 16),
            );
        }
        offset += 8 + size + (size & 1);
    }
    (loop_count.unwrap(), durations)
}

fn append_webp_xmp(path: &std::path::Path, metadata: &[u8]) {
    let mut encoded = fs::read(path).unwrap();
    let mut offset = 12;
    while offset < encoded.len() {
        let size = u32::from_le_bytes(encoded[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if &encoded[offset..offset + 4] == b"VP8X" {
            encoded[offset + 8] |= 0x04;
            break;
        }
        offset += 8 + size + (size & 1);
    }
    encoded.extend_from_slice(b"XMP ");
    encoded.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    encoded.extend_from_slice(metadata);
    if !metadata.len().is_multiple_of(2) {
        encoded.push(0);
    }
    let riff_size = (encoded.len() - 8) as u32;
    encoded[4..8].copy_from_slice(&riff_size.to_le_bytes());
    fs::write(path, encoded).unwrap();
}

fn webp_chunk(path: &std::path::Path, expected: &[u8; 4]) -> Option<Vec<u8>> {
    let encoded = fs::read(path).unwrap();
    let mut offset = 12;
    while offset < encoded.len() {
        let size = u32::from_le_bytes(encoded[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if &encoded[offset..offset + 4] == expected {
            return Some(encoded[offset + 8..offset + 8 + size].to_vec());
        }
        offset += 8 + size + (size & 1);
    }
    None
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
fn transforms_transparent_webp_and_avif_through_cli() {
    for extension in ["webp", "avif"] {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join(format!("input.{extension}"));
        let output = directory.path().join(format!("output.{extension}"));
        write_transparent_image(&input);

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
                "invert",
            ])
            .assert()
            .success();

        let decoded = image::open(&output).unwrap().into_rgba8();
        assert_eq!(decoded.dimensions(), (4, 2));
        assert!(decoded.pixels().any(|pixel| pixel[3] < 255));

        Command::cargo_bin("databender")
            .unwrap()
            .args([
                "transform",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--filter",
                "invert",
                "--protect-output",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("refusing to overwrite"));
    }
}

#[test]
fn transforms_animated_webp_preserving_timing_loop_and_alpha() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");
    let input = directory.path().join("input.webp");
    let output = directory.path().join("output.webp");
    ImageBuffer::from_pixel(4, 2, Rgba([10_u8, 20, 30, 96]))
        .save(&first)
        .unwrap();
    ImageBuffer::from_pixel(4, 2, Rgba([80_u8, 90, 100, 160]))
        .save(&second)
        .unwrap();
    run_ffmpeg(&[
        "-v".as_ref(),
        "error".as_ref(),
        "-y".as_ref(),
        "-loop".as_ref(),
        "1".as_ref(),
        "-framerate".as_ref(),
        "1000".as_ref(),
        "-t".as_ref(),
        "0.125".as_ref(),
        "-i".as_ref(),
        first.as_os_str(),
        "-loop".as_ref(),
        "1".as_ref(),
        "-framerate".as_ref(),
        "1000".as_ref(),
        "-t".as_ref(),
        "0.340".as_ref(),
        "-i".as_ref(),
        second.as_os_str(),
        "-filter_complex".as_ref(),
        "[0:v][1:v]concat=n=2:v=1:a=0".as_ref(),
        "-fps_mode".as_ref(),
        "vfr".as_ref(),
        "-lossless".as_ref(),
        "1".as_ref(),
        "-loop".as_ref(),
        "7".as_ref(),
        input.as_os_str(),
    ]);
    let xmp = b"<x:xmpmeta>animated fixture</x:xmpmeta>";
    append_webp_xmp(&input, xmp);

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
            "invert",
        ])
        .assert()
        .success();

    assert_eq!(webp_animation_timing(&input), (7, vec![125, 340]));
    assert_eq!(webp_animation_timing(&output), (7, vec![125, 340]));
    assert_eq!(
        webp_chunk(&output, b"XMP ").as_deref(),
        Some(xmp.as_slice())
    );
    let decoded = directory.path().join("decoded.rgba");
    run_ffmpeg(&[
        "-v".as_ref(),
        "error".as_ref(),
        "-y".as_ref(),
        "-i".as_ref(),
        output.as_os_str(),
        "-fps_mode".as_ref(),
        "passthrough".as_ref(),
        "-pix_fmt".as_ref(),
        "rgba".as_ref(),
        "-f".as_ref(),
        "rawvideo".as_ref(),
        decoded.as_os_str(),
    ]);
    let frames = fs::read(decoded).unwrap();
    assert_eq!(frames.len(), 4 * 2 * 4 * 2);
    assert_eq!(&frames[..4], &[245, 235, 225, 96]);
    assert_eq!(&frames[4 * 2 * 4..4 * 2 * 4 + 4], &[175, 165, 155, 160]);
}

#[test]
fn rejects_malformed_animated_webp_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("broken.webp");
    let output = directory.path().join("output.webp");
    let mut encoded = b"RIFF\0\0\0\0WEBP".to_vec();
    encoded.extend_from_slice(b"ANMF\x10\0\0\0");
    encoded.extend_from_slice(&[0; 15]);
    let riff_size = (encoded.len() - 8) as u32;
    encoded[4..8].copy_from_slice(&riff_size.to_le_bytes());
    fs::write(&input, encoded).unwrap();

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--filter",
            "invert",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid WebP"));

    assert!(!output.exists());
}

#[test]
fn transforms_avif_sequence_preserving_timing_and_loop_count() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");
    let input = directory.path().join("input.avif");
    let output = directory.path().join("output.avif");
    ImageBuffer::from_pixel(16, 8, Rgba([10_u8, 20, 30, 255]))
        .save(&first)
        .unwrap();
    ImageBuffer::from_pixel(16, 8, Rgba([80_u8, 90, 100, 255]))
        .save(&second)
        .unwrap();
    run_ffmpeg(&[
        "-v".as_ref(),
        "error".as_ref(),
        "-framerate".as_ref(),
        "10".as_ref(),
        "-i".as_ref(),
        first.as_os_str(),
        "-framerate".as_ref(),
        "10".as_ref(),
        "-i".as_ref(),
        second.as_os_str(),
        "-filter_complex".as_ref(),
        "[0:v]settb=1/10,setpts=0[f0];[1:v]settb=1/10,setpts=3[f1];[f0][f1]interleave=n=2".as_ref(),
        "-fps_mode".as_ref(),
        "vfr".as_ref(),
        "-enc_time_base".as_ref(),
        "1/10".as_ref(),
        "-loop".as_ref(),
        "7".as_ref(),
        input.as_os_str(),
    ]);

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
            "invert",
        ])
        .assert()
        .success();

    assert!(output.exists());
    assert_ne!(fs::read(input).unwrap(), fs::read(output).unwrap());
}

#[test]
fn transforms_avif_sequence_preserving_auxiliary_alpha() {
    if !avifenc_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");
    let input = directory.path().join("input-alpha.avif");
    let output = directory.path().join("output-alpha.avif");
    ImageBuffer::from_pixel(16, 8, Rgba([10_u8, 20, 30, 64]))
        .save(&first)
        .unwrap();
    ImageBuffer::from_pixel(16, 8, Rgba([80_u8, 90, 100, 160]))
        .save(&second)
        .unwrap();
    run_avifenc(&[
        "-q".as_ref(),
        "100".as_ref(),
        "--qalpha".as_ref(),
        "100".as_ref(),
        "--timescale".as_ref(),
        "10".as_ref(),
        "--repetition-count".as_ref(),
        "6".as_ref(),
        "--duration".as_ref(),
        "3".as_ref(),
        first.as_os_str(),
        "--duration".as_ref(),
        "1".as_ref(),
        second.as_os_str(),
        input.as_os_str(),
    ]);

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
            "invert",
        ])
        .assert()
        .success();

    let alpha = directory.path().join("alpha.gray");
    run_ffmpeg(&[
        "-v".as_ref(),
        "error".as_ref(),
        "-y".as_ref(),
        "-i".as_ref(),
        output.as_os_str(),
        "-map".as_ref(),
        "0:v:3".as_ref(),
        "-fps_mode".as_ref(),
        "passthrough".as_ref(),
        "-pix_fmt".as_ref(),
        "gray".as_ref(),
        "-f".as_ref(),
        "rawvideo".as_ref(),
        alpha.as_os_str(),
    ]);
    let alpha = fs::read(alpha).unwrap();
    assert_eq!(alpha.len(), 16 * 8 * 2);
    assert_eq!(alpha[0], 64);
    assert_eq!(alpha[16 * 8], 160);
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
fn transforms_with_versioned_preset_and_protects_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("preset.png");
    let expected = directory.path().join("expected.png");
    let config = directory.path().join("databender.toml");
    write_png(&input);
    fs::write(
        &config,
        r#"
version = 1

[presets.shift]
filters = ["invert"]
seed = 77
output_policy = "protect"
"#,
    )
    .unwrap();

    let mut preset = Command::cargo_bin("databender").unwrap();
    preset
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--preset",
            "shift",
            "--filter",
            "channel-shift:pixels=1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 77"));

    let mut direct = Command::cargo_bin("databender").unwrap();
    direct
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            expected.to_str().unwrap(),
            "--seed",
            "77",
            "--filter",
            "invert",
            "--filter",
            "channel-shift:pixels=1",
        ])
        .assert()
        .success();
    assert_eq!(fs::read(&output).unwrap(), fs::read(expected).unwrap());

    let mut protected = Command::cargo_bin("databender").unwrap();
    protected
        .args([
            "transform",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--preset",
            "shift",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to overwrite"));
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
            "huffman-glitch:swaps=128,intensity=1,target=all,engine=table,mode=run-remap,preserve_size=true",
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
