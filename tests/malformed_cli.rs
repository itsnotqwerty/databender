use std::fs;

use assert_cmd::Command;

#[test]
fn rejects_malformed_v04_containers_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let cases: &[(&str, &[u8], &str)] = &[
        ("broken.webp", b"RIFF\x10\0\0\0WEBPVP8L", "invert"),
        ("broken.avif", b"\0\0\0\x18ftypavif\0\0\0\0mif1", "invert"),
        ("broken.mp4", b"\0\0\0\x18ftypisom\0\0\0\0isom", "invert"),
        ("broken.ogg", b"OggS\0\x02", "volume:gain=1.1"),
        ("broken.mkv", b"\x1a\x45\xdf\xa3\x9f", "invert"),
    ];

    for (name, contents, filter) in cases {
        let input = directory.path().join(name);
        let output = directory.path().join(format!("output-{name}"));
        fs::write(&input, contents).unwrap();

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
            .failure();
        assert!(!output.exists(), "published output for {name}");
    }
}
