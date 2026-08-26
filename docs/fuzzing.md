# Fuzzing

Databender keeps coverage-guided fuzzing in the isolated `fuzz` package. Install cargo-fuzz with a nightly toolchain, then run one target at a time:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run encoded_video
cargo +nightly fuzz run encoded_audio
cargo +nightly fuzz run plugin_abi
cargo +nightly fuzz run image_containers
```

Use bounded campaigns in automation or before release:

```bash
cargo +nightly fuzz run encoded_video -- -max_total_time=300
cargo +nightly fuzz run encoded_audio -- -max_total_time=300
cargo +nightly fuzz run plugin_abi -- -max_total_time=300
cargo +nightly fuzz run image_containers -- -max_total_time=300
```

`encoded_video` feeds arbitrary H.264, H.265, VP8, VP9, and AV1 packet bytes and controls into the structure-aware mutator. Successful mutations must preserve length and report bounded impact.

`encoded_audio` parses arbitrary MP3 and Ogg bytes. Successful parses proceed through native mutation; successful repairs must remain length-preserving and reparsable.

`plugin_abi` strictly deserializes arbitrary manifests, commands, and events, invokes semantic validation, and feeds the same bytes through Wasmtime module compilation plus the import/export policy boundary.

`image_containers` dispatches arbitrary bytes independently through JPEG metadata, baseline/progressive coefficient reconstruction, bounded coefficient mutation and repair, PNG metadata parsing, WebP chunk and animation parsing, and AVIF item/property and sequence parsing. Selecting the parser independently of magic bytes lets mutation reach deeper malformed states.

Crashes and timeouts are retained by cargo-fuzz under `fuzz/artifacts/<target>`. Minimize a reproducer before adding it permanently:

```bash
cargo +nightly fuzz tmin encoded_video fuzz/artifacts/encoded_video/CRASH
```

Add minimized regressions to the target corpus and a deterministic unit test near the owning parser. Do not check generated artifact directories into source control. CI compiles every fuzz target on Linux; scheduled or local campaigns provide runtime coverage.

CI compiles all parser, mutation, repair, progressive JPEG, and plugin ABI/runtime fuzz boundaries. Bounded release campaigns remain operational work; promoted crashes become deterministic regression tests beside the owning parser.
