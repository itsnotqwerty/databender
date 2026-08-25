# Databender

Databender is a Linux-first Rust library and CLI for deterministic, structure-aware media corruption. It transforms JPEG, PNG, WebP, and AVIF images; WAV, MP3, Ogg Vorbis, and Ogg Opus audio; and MP4 or Matroska video and audio streams.

The implemented foundation parses typed filter parameters, detects formats from content, models filters as an ordered pipeline, and rejects incompatible combinations before media processing begins. Prepared transforms validate candidates and publish them atomically.

## Current Status

| Format | Status | Planned processing |
| --- | --- | --- |
| JPEG | Pixel and Huffman filters available | Decoded pixels and DHT symbol mutation |
| PNG | Pixel and payload filters available | Decoded pixels and non-interlaced scanlines |
| WAV | PCM and payload filters available | 8/16/24/32-bit integer PCM samples and `data` bytes |
| MP3 | Main-data and FFmpeg audio filters available | Bounded encoded mutation, high-pass, low-pass, echo, and volume |
| MP4 | Packet, pixel, PCM, and FFmpeg filters available | Selected video and all audio streams |
| WebP | Pixel filters available | Still and animated images; timing, loops, alpha, ICC, EXIF, and XMP preserved |
| AVIF | Pixel filters available | Still and sequenced images; timing, loops, and auxiliary alpha preserved |
| Ogg | Packet, PCM, and FFmpeg audio filters available | Bounded Vorbis/Opus packet mutation and all audio streams |
| Matroska | Packet, pixel, PCM, and FFmpeg filters available | Selected video and all audio streams |

## Development

Build and test with a stable Rust toolchain:

```bash
cargo build
cargo test
```

MP3, Ogg, MP4, and Matroska validation and compressed processing use installed `ffmpeg` and `ffprobe`. The library provides a timeout-aware runner that invokes them without a shell, nulls standard input, and bounds captured output. MP3 additionally supports native structure-aware main-data mutation. Typed high-pass, low-pass, echo, and volume filters compile into an allowlisted audio graph; hue, equalization, and lag compile into an allowlisted video graph. Outputs are fully decoded before publication. MP4 and Matroska map one primary video stream and every audio stream, apply native image filters frame-by-frame and PCM noise to 16-bit WAV intermediates, and validate stream geometry and basic metadata.

Executables are discovered through `PATH` by default. Set `DATABENDER_FFMPEG` and `DATABENDER_FFPROBE` to explicit executable paths when they are installed elsewhere. Pipeline preflight reports missing tools, required encoders, and typed FFmpeg filters before media processing begins.

Inspect the available formats and filter names:

```bash
cargo run -- list-formats
cargo run -- list-filters
```

`list-filters` groups filters by compatible codec and processing domain.
Discovery checks `ffmpeg`, `ffprobe`, and the required encoders. FFmpeg-backed formats with missing components are reported as unavailable instead of advertising unusable filters.

Discover import-free WebAssembly plugin bundles from explicit directories:

```bash
cargo run -- list-plugins --plugin-dir plugins \
	--disable-plugin example.experimental
```

Each bundle pairs `<name>.plugin.json` with `<name>.wasm`. Discovery reports enabled state, manifest/ABI and Wasm export compatibility, declared filters, and manifest provenance without running the module. Transform, batch, and TUI commands accept the same repeatable `--plugin-dir` and `--disable-plugin` controls; `--plugin-config PATH` loads global registry settings.

Validate an ordered pipeline without reading or writing media:

```bash
cargo run -- plan \
	--format png \
	--seed 42 \
	--filter channel-shift \
	--filter byte-noise
```

Transform an image with ordered pixel filters:

```bash
cargo run -- transform input.png \
	--output output.png \
	--seed 42 \
	--filter channel-shift:pixels=8 \
	--filter scanline-displacement:max_shift=24 \
	--filter pixel-sort:threshold=128
```

PNG pipelines can mix pixel and payload filters in order:

```bash
cargo run -- transform input.png \
	--output output.png \
	--seed 42 \
	--filter byte-noise:probability=0.08 \
	--filter channel-shift:pixels=8 \
	--filter byte-swap:count=24
```

For severe JPEG corruption, mutate the AC Huffman symbol tables. Baseline JPEGs are analyzed in real time so frequently used symbols can drive the remapping. Higher swap counts and intensity generally produce more block displacement and coefficient collapse:

```bash
cargo run -- transform input.jpg \
	--output output.jpg \
	--seed 42 \
	--filter huffman-glitch:swaps=128,intensity=0.75,target=luma-ac,engine=table,mode=run-remap
```

`engine=table` is the stable compatibility engine and remains the default. It mutates DHT symbol mappings in place, so every occurrence of a remapped symbol changes globally without decoding, targeting, or re-encoding individual quantized coefficients. A future coefficient engine will instead reconstruct scan coefficients and support scan, component, and frequency targeting; `engine=coefficient` is rejected until that implementation exists. `target` accepts `all`, `luma-ac`, or `chroma-ac` and defaults to `luma-ac`. Intensity is deliberately nonlinear: low values perform very few swaps among rarely decoded symbols with nearby run lengths, while high values admit broader, more frequently used pairs. The default `run-remap` mode preserves amplitude size, which keeps entropy bit consumption relatively stable. `symbol-remap` is more chaotic; set `preserve_size=false` to allow amplitude-size changes, with a higher chance that candidate validation rejects the result. Progressive and unsupported scan layouts use deterministic table-aware fallback remapping because refinement scans require a separate coefficient decoder.

WAV pipelines can mix sample-aware bounded noise and length-preserving payload filters. RIFF chunks outside `data` remain byte-identical:

```bash
cargo run -- transform input.wav \
	--output output.wav \
	--seed 42 \
	--filter byte-swap:count=128 \
	--filter audio-noise:probability=0.1,amplitude=0.4
```

MP3 pipelines can alternate bounded native main-data mutation and typed FFmpeg effects while preserving basic container metadata:

```bash
cargo run -- transform input.mp3 \
	--output output.mp3 \
	--seed 42 \
	--filter mp3-main-data-noise:byte_budget=8,start_frame=2,frame_count=20,intensity=0.125 \
	--filter high-pass:frequency=200 \
	--filter echo:delay_ms=80,decay=0.35 \
	--filter volume:gain=1.2
```

MP3 does not yet support decoded PCM filters such as `audio-noise`.

The Layer III parser protects frame headers, optional CRC fields, side information, and metadata. Main-data mutation skips CRC-protected frames, selects bytes deterministically without replacement, reparses the candidate, validates stream geometry, and requires a complete decode. `plan` and batch `--dry-run --json` report the upper-bound impact; see [MP3 structural parsing](docs/mp3-structure.md).

Ogg pipelines support bounded packet payload mutation before or after decoded audio effects:

```bash
cargo run -- transform input.ogg \
	--output output.ogg \
	--seed 42 \
	--filter ogg-packet-noise:byte_budget=8,start_packet=2,packet_count=16,intensity=0.125,max_decode_errors=0 \
	--filter volume:gain=0.9
```

Vorbis and Opus identification and setup packets are protected. Audio packets may span pages; touched page CRCs are rebuilt, logical-stream sequence continuity is validated, and candidates must reparse and fully decode before publication. A `packet_count` of `0` selects all remaining eligible packets. `max_decode_errors` defaults to `0` and limits non-empty FFmpeg error-level diagnostic lines from the complete decode; truncated diagnostics always reject. Pipelines with multiple packet filters use the strictest declared limit.

Explicit expert graphs can use additional installed FFmpeg filters without changing the typed defaults:

```bash
cargo run -- transform input.mp3 --output output.mp3 --seed 42 \
	--filter 'expert-audio-graph:volume=0.5,aecho=0.8:0.9:20:0.2'
```

Use `expert-video-graph:<fragment>` for video. Expert fragments are bounded single chains; Databender rejects labels, multiple chains, shell interpolation, protocols, and filters/options that access external resources. Filter availability is checked during preflight, accepted text is passed directly to FFmpeg without a shell, and resolved graphs are recorded in plans and batch reports as environment-dependent. See [expert graphs](docs/expert-graphs.md) for the complete policy.

MP4 pipelines can interleave typed video and audio effects. Effects retain their order within each target stream:

```bash
cargo run -- transform input.mp4 \
	--output output.mp4 \
	--seed 42 \
	--video-stream 0 \
	--filter scanline-displacement:max_shift=16 \
	--filter hue:degrees=60 \
	--filter audio-noise:probability=0.08,amplitude=0.2 \
	--filter high-pass:frequency=200 \
	--filter lag:frames=3 \
	--filter volume:gain=1.2
```

Native and FFmpeg stages use lossless FFV1 or PCM intermediates so their relative order is retained independently for video and audio. Repeat `--video-stream INDEX` to transform one or more zero-based MP4 or Matroska video streams; unselected video streams are copied in source order, and omitting the option selects stream 0. Preserved MP4 metadata includes global title, artist, album, comment, genre, date, creation time, and copyright plus title and language for every video and audio stream. MP4 subtitles, chapters, attachments, and other metadata classes are not processed. Matroska additionally copies subtitle and attachment streams plus chapters. Validation compares ordered video dimensions, average frame rates, decoded frame counts, all supported stream metadata, audio topology, auxiliary codecs, and chapter timing before completely decoding every video and audio stream prior to publication.

MP4 and Matroska also support packet-only H.264, H.265, VP8, VP9, and AV1 payload mutation. MP4 verifies direct packet offsets by SHA-256; Matroska resolves unlaced EBML block payloads by size and SHA-256. Both preserve container bookkeeping through equal-length writes and reject mixed packet/decoded pipelines.

```bash
cargo run -- transform input.mp4 \
	--output output.mp4 \
	--seed 42 \
	--filter video-packet-noise:byte_budget=8,start_packet=2,packet_count=16,frame_type=delta,intensity=0.125,max_frame_loss=0
```

Named presets use a versioned TOML file. Preset filters run first; explicit `--filter` arguments append to them. An explicit `--seed` overrides the preset seed, while command-line output protection can tighten but not weaken the preset policy:

```toml
version = 1
plan_version = 1

[plugins]
directories = ["plugins"]
disabled = ["example.experimental"]

[presets.shift]
filters = ["invert", "channel-shift:pixels=8"]
seed = 42
output_policy = "protect"

[presets.shift.plugins]
directories = ["project-plugins"]
disabled = ["example.noisy"]
```

`plan_version` is required when a preset uses encoded mutation or an expert FFmpeg graph. It must match the version printed by `plan`; this prevents a preset from silently adopting changed codec or graph semantics.

```bash
cargo run -- transform input.png --output output.png \
	--config databender.toml --preset shift
```

Batch mode recursively expands explicit files or directories and automatically excludes files whose extension is not supported. Extension matching is case-insensitive; included files are still validated by content signature. Flat layout uses source file names and rejects collisions before processing; mirrored layout retains paths relative to `--root`. `--jobs` bounds concurrent files, while output reports remain in deterministic input order:

```bash
cargo run -- batch media --output-dir bent \
	--layout mirrored --root media --jobs 4 --seed 42 \
	--video-stream 0 --filter invert

cargo run -- batch media --output-dir bent \
	--seed 42 --filter invert --dry-run --json
```

Each input receives a deterministic path-derived seed. Native container filters derive tagged child seeds from stream, stage, and frame identities, so concurrent scheduling cannot alter output. Batch failures are reported per file after all scheduled work finishes, and any failure produces a nonzero aggregate exit status.

Use `--manifest state.json` to atomically save a versioned report. A later run with the same resolved inputs, content, filters, video stream selectors, seed, output mapping, and protection policy can pass `--resume state.json`; successful items with existing outputs are skipped, while failed or missing outputs run again. Resuming updates the same manifest unless a different `--manifest` path is supplied. Changed plans or input content are rejected before processing.

Omit `--seed` to generate and print one. Existing destinations are replaced by default; use `--protect-output` to refuse replacement. The input file is never overwritten.

Additional JPEG/PNG pixel filters are `brightness`, `contrast`, `saturation`, `hue-rotate`, `posterize`, `invert`, and seeded `row-dropout`. `huffman-glitch` is JPEG-only and defaults to 32 swaps, full intensity, luma AC, run remapping, and preserved amplitude size. Run `cargo run -- list-filters` for codec compatibility. Byte filters such as `byte-swap` operate on PNG scanline payloads and are rejected for JPEG.

Pixel transforms preserve supported basic metadata: JPEG APP1/EXIF, APP2/ICC, and comments; PNG color/profile, resolution, EXIF, text, and timestamp chunks; and WebP ICC, EXIF, and XMP chunks. Pixel-layout-dependent PNG chunks are intentionally not transplanted. Animated WebP is processed frame-by-frame with deterministic frame seeds while preserving frame order, millisecond durations, loop count, alpha, and supported WebP metadata. AVIF sequences preserve frame order, rational frame durations, finite or infinite loop count, and auxiliary alpha tracks. AVIF metadata preservation is not yet implemented. Alpha sequence output uses `avifenc`, configurable with `DATABENDER_AVIFENC`; opaque sequences continue to use FFmpeg.

Browse media and choose an output directory in the terminal interface:

```bash
cargo run -- tui media --output-dir bent
```

The input browser recursively excludes files without a supported extension before probing their content. The bottom-row hint keeps the primary controls visible. Press `?` for the complete in-TUI controls guide; `?`, Escape, or `q` closes the guide without triggering the hidden action. Press `f` to list supported formats and filters or `v` to expand the selected media preview; `v`, Escape, or `q` closes the expanded view. Use Up/Down or `k`/`j` to select media, Home/End to jump, `p` to enter a typed filter specification, Left/Right to select a pipeline entry, `e` to edit the highlighted entry in place, `d` to remove it, and `o` to edit the output directory. Resolved default options are omitted from pipeline rows; options explicitly entered in the TUI, CLI, or a preset remain visible when edited. Use `[` and `]` to select a discovered plugin and `t` to toggle its enabled state. Enter accepts an edit, Escape cancels it, and `q` exits when overlays are closed. Pipeline edits are preflighted against the selected codec immediately. Initialize the editor with `--config` and `--preset`, or repeated `--filter` options. Choose `--theme standard`, `--theme high-contrast`, or `--theme monochrome`; `--no-color` and the `NO_COLOR` environment variable force monochrome output.

Press `s` to start the queue, Space to pause or resume intake, `c` to cancel active work, and `r` to resume cancelled work or retry failed files. The queue preserves deterministic per-file seeds, reports current and completed files, and retains the newest 100 log entries.

The preview pane decodes still images through the native image path, samples the first video frame through the FFmpeg runner, and renders a 48x24 sample as solid RGB terminal cells backed by hexadecimal luminance characters. Coloring both foreground and background preserves true dark and black pixels. Compact panes resample the complete image instead of clipping it, while expanded mode uses the full terminal body. Preview cells are doubled horizontally to compensate for terminal proportions. Monochrome mode retains the hexadecimal characters without color. Audio previews summarize at most ten seconds of mono decoded audio. Preview failures remain isolated from transform planning and publication.

The same seed and native pipeline will produce deterministic stage seeds. FFmpeg-backed stages preserve the resolved plan and parameters, but compressed output is not promised byte-identical across FFmpeg versions.

See [the design](docs/design.md), [specification](docs/spec.md), [threat model](docs/threat-model.md), [fuzzing guide](docs/fuzzing.md), [expert graphs](docs/expert-graphs.md), [encoded video](docs/encoded-video.md), [MP3 structural parsing](docs/mp3-structure.md), [plugin ABI](docs/plugin-abi.md), [plugin SDK](docs/plugin-sdk.md), [operational limits](docs/operations.md), [platform support](docs/platforms.md), [v0.6 migration guide](docs/migration-v0.6.md), [usability protocol](docs/usability-sessions.md), and [roadmap](docs/roadmap.md) for implementation contracts, measured resource scaling, portability guarantees, and release gates.