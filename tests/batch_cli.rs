use std::fs;

use assert_cmd::Command;
use image::{ImageBuffer, Rgb, Rgba};
use predicates::prelude::*;

fn write_png(path: &std::path::Path) {
    let image = ImageBuffer::from_fn(4, 2, |x, y| {
        Rgba([(x * 50) as u8, (y * 100) as u8, (x * 20) as u8, 255])
    });
    image.save(path).unwrap();
}

fn write_jpeg(path: &std::path::Path) {
    let image = ImageBuffer::from_pixel(4, 2, Rgb([20_u8, 40, 60]));
    image.save(path).unwrap();
}

#[test]
fn recursively_transforms_mirrored_inputs_deterministically() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("inputs");
    let nested = root.join("nested");
    let first_output = directory.path().join("first");
    let second_output = directory.path().join("second");
    fs::create_dir_all(&nested).unwrap();
    write_png(&root.join("one.png"));
    write_png(&nested.join("two.png"));

    for output in [&first_output, &second_output] {
        let mut command = Command::cargo_bin("databender").unwrap();
        command
            .args([
                "batch",
                root.to_str().unwrap(),
                "--output-dir",
                output.to_str().unwrap(),
                "--layout",
                "mirrored",
                "--root",
                root.to_str().unwrap(),
                "--jobs",
                "2",
                "--seed",
                "42",
                "--filter",
                "row-dropout:probability=0.5",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("seed="));
    }

    assert_eq!(
        fs::read(first_output.join("one.png")).unwrap(),
        fs::read(second_output.join("one.png")).unwrap()
    );
    assert_eq!(
        fs::read(first_output.join("nested/two.png")).unwrap(),
        fs::read(second_output.join("nested/two.png")).unwrap()
    );
}

#[test]
fn rejects_flat_output_collisions_before_processing() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    let output = directory.path().join("output");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    write_png(&left.join("same.png"));
    write_png(&right.join("same.png"));

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "batch",
            left.to_str().unwrap(),
            right.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--filter",
            "invert",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "multiple inputs resolve to output",
        ));
    assert!(!output.exists());
}

#[test]
fn reports_partial_failure_after_successful_items_finish() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("inputs");
    let output = directory.path().join("output");
    fs::create_dir_all(&input).unwrap();
    write_png(&input.join("valid.png"));
    write_jpeg(&input.join("incompatible.jpg"));

    let mut command = Command::cargo_bin("databender").unwrap();
    command
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--jobs",
            "2",
            "--seed",
            "42",
            "--filter",
            "byte-swap:count=1",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("valid.png"))
        .stderr(predicate::str::contains("incompatible.jpg"))
        .stderr(predicate::str::contains(
            "batch completed with 1 failed item(s)",
        ));

    assert!(output.join("valid.png").exists());
    assert!(!output.join("incompatible.jpg").exists());
}

#[test]
fn emits_json_dry_run_without_creating_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.png");
    let output = directory.path().join("output");
    write_png(&input);

    let assertion = Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "invert",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let document: serde_json::Value =
        serde_json::from_slice(&assertion.get_output().stdout).unwrap();

    assert_eq!(document["items"][0]["executed"], false);
    assert_eq!(document["items"][0]["error"], serde_json::Value::Null);
    assert!(!output.exists());
}

#[test]
fn resumes_compatible_manifest_and_retries_missing_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("inputs");
    let output = directory.path().join("output");
    let manifest = directory.path().join("batch-manifest.json");
    fs::create_dir_all(&input).unwrap();
    write_png(&input.join("one.png"));
    write_png(&input.join("two.png"));

    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "invert",
            "--manifest",
            manifest.to_str().unwrap(),
        ])
        .assert()
        .success();
    let initial: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(initial["version"], 3);
    assert_eq!(initial["plan_version"], 1);
    assert_eq!(initial["items"].as_array().unwrap().len(), 2);

    let incompatible_manifest = directory.path().join("incompatible-manifest.json");
    let mut incompatible = initial.clone();
    incompatible["plan_version"] = serde_json::json!(99);
    fs::write(
        &incompatible_manifest,
        serde_json::to_vec_pretty(&incompatible).unwrap(),
    )
    .unwrap();
    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "invert",
            "--resume",
            incompatible_manifest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unsupported pipeline plan version 99; expected 1",
        ));

    fs::remove_file(output.join("two.png")).unwrap();
    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "invert",
            "--resume",
            manifest.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("resumed\t"))
        .stdout(predicate::str::contains("ok\t"));
    assert!(output.join("two.png").exists());

    write_png(&input.join("three.png"));
    Command::cargo_bin("databender")
        .unwrap()
        .args([
            "batch",
            input.to_str().unwrap(),
            "--output-dir",
            output.to_str().unwrap(),
            "--seed",
            "42",
            "--filter",
            "invert",
            "--resume",
            manifest.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "resume manifest does not match the resolved batch plan",
        ));
    assert!(!output.join("three.png").exists());
}
