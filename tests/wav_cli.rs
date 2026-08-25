use std::fs;

use assert_cmd::Command;
use databender::codecs::wav;
use predicates::prelude::*;

fn write_wav(path: &std::path::Path, aligned: bool) {
    let samples = (-16_i16..16).flat_map(i16::to_le_bytes).collect::<Vec<_>>();
    let data = if aligned {
        samples.as_slice()
    } else {
        &samples[..samples.len() - 1]
    };
    let mut chunks = Vec::new();
    chunks.extend_from_slice(b"fmt ");
    chunks.extend_from_slice(&16_u32.to_le_bytes());
    chunks.extend_from_slice(&1_u16.to_le_bytes());
    chunks.extend_from_slice(&1_u16.to_le_bytes());
    chunks.extend_from_slice(&8_000_u32.to_le_bytes());
    chunks.extend_from_slice(&16_000_u32.to_le_bytes());
    chunks.extend_from_slice(&2_u16.to_le_bytes());
    chunks.extend_from_slice(&16_u16.to_le_bytes());
    chunks.extend_from_slice(b"LIST");
    chunks.extend_from_slice(&4_u32.to_le_bytes());
    chunks.extend_from_slice(b"INFO");
    chunks.extend_from_slice(b"data");
    chunks.extend_from_slice(&(data.len() as u32).to_le_bytes());
    chunks.extend_from_slice(data);
    if data.len() & 1 == 1 {
        chunks.push(0);
    }

    let mut encoded = b"RIFF".to_vec();
    encoded.extend_from_slice(&((chunks.len() + 4) as u32).to_le_bytes());
    encoded.extend_from_slice(b"WAVE");
    encoded.extend_from_slice(&chunks);
    fs::write(path, encoded).unwrap();
}

#[test]
fn transforms_wav_with_ordered_pcm_and_payload_filters() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.wav");
    let output = directory.path().join("output.wav");
    write_wav(&input, true);
    let original = fs::read(&input).unwrap();
    let original_layout = wav::parse(&original).unwrap();

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
            "byte-swap:count=8",
            "--filter",
            "audio-noise:probability=1,amplitude=0.5",
            "--filter",
            "byte-repeat:count=8",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("seed: 42"));

    let transformed = fs::read(output).unwrap();
    let transformed_layout = wav::parse(&transformed).unwrap();
    assert_eq!(transformed_layout, original_layout);
    assert_eq!(
        &transformed[..transformed_layout.data.start],
        &original[..original_layout.data.start]
    );
    assert_ne!(
        &transformed[transformed_layout.data],
        &original[original_layout.data]
    );
}

#[test]
fn rejects_misaligned_wav_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.wav");
    let output = directory.path().join("output.wav");
    write_wav(&input, false);

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
            "data chunk does not contain whole sample frames",
        ));

    assert!(!output.exists());
}
