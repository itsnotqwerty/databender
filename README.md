# Databender

Databender is a Linux-first Rust library and CLI for deterministic, structure-aware media corruption. It transforms JPEG, PNG, WebP, and AVIF images; WAV, MP3, Ogg Vorbis, and Ogg Opus audio; and MP4 or Matroska video and audio streams.

The implemented foundation parses typed filter parameters, detects formats from content, models filters as an ordered pipeline, and rejects incompatible combinations before media processing begins. Prepared transforms validate candidates and publish them atomically.

## Current Status

| Format | Status | Planned processing |
| --- | --- | --- |
| JPEG | Pixel and Huffman filters available | Decoded pixels and DHT symbol mutation |
| PNG | Pixel and payload filters available | Decoded pixels and non-interlaced scanlines |
| WAV | PCM and payload filters available | 8/16/24/32-bit integer PCM samples and `data` bytes |
| MP3 | FFmpeg audio filters available | High-pass, low-pass, echo, and volume |
| MP4 | Pixel, PCM, and FFmpeg filters available | One video and all audio streams |
| WebP | Pixel filters available | Still images with dimensions, alpha, ICC, EXIF, and XMP preserved |
| AVIF | Pixel filters available | Still images with dimensions and alpha preserved |
| Ogg | PCM and FFmpeg audio filters available | All Vorbis or Opus streams |
| Matroska | Pixel, PCM, and FFmpeg filters available | One video and all audio streams |

## Development

Build and test with a stable Rust toolchain:

```bash
cargo build
cargo test
```

MP3, Ogg, MP4, and Matroska processing use installed `ffmpeg` and `ffprobe`. The library provides a timeout-aware runner that invokes them without a shell, nulls standard input, and bounds captured output. Typed high-pass, low-pass, echo, and volume filters compile into an allowlisted audio graph; hue, equalization, and lag compile into an allowlisted video graph. Outputs are fully decoded before publication. MP4 and Matroska map one primary video stream and every audio stream, apply native image filters frame-by-frame and PCM noise to 16-bit WAV intermediates, and validate stream geometry and basic metadata.

Inspect the available formats and filter names:

```bash
cargo run -- list-formats
cargo run -- list-filters
```

`list-filters` groups filters by compatible codec and processing domain.

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
	--filter huffman-glitch:swaps=128,intensity=0.75,target=luma-ac,mode=run-remap
```

`target` accepts `all`, `luma-ac`, or `chroma-ac` and defaults to `luma-ac`. Intensity is deliberately nonlinear: low values perform very few swaps among rarely decoded symbols with nearby run lengths, while high values admit broader, more frequently used pairs. The default `run-remap` mode preserves amplitude size, which keeps entropy bit consumption relatively stable. `symbol-remap` is more chaotic; set `preserve_size=false` to allow amplitude-size changes, with a higher chance that candidate validation rejects the result. Progressive and unsupported scan layouts use deterministic table-aware fallback remapping because refinement scans require a separate coefficient decoder.

WAV pipelines can mix sample-aware bounded noise and length-preserving payload filters. RIFF chunks outside `data` remain byte-identical:

```bash
cargo run -- transform input.wav \
	--output output.wav \
	--seed 42 \
	--filter byte-swap:count=128 \
	--filter audio-noise:probability=0.1,amplitude=0.4
```

MP3 pipelines apply ordered typed effects through FFmpeg while preserving basic container metadata:

```bash
cargo run -- transform input.mp3 \
	--output output.mp3 \
	--seed 42 \
	--filter high-pass:frequency=200 \
	--filter echo:delay=80,decay=0.35 \
	--filter volume:gain=1.2
```

MP3 does not yet support decoded PCM filters such as `audio-noise`.

MP4 pipelines can interleave typed video and audio effects. Effects retain their order within each target stream:

```bash
cargo run -- transform input.mp4 \
	--output output.mp4 \
	--seed 42 \
	--filter scanline-displacement:max_shift=16 \
	--filter hue:degrees=60 \
	--filter audio-noise:probability=0.08,amplitude=0.2 \
	--filter high-pass:frequency=200 \
	--filter lag:frames=3 \
	--filter volume:gain=1.2
```

Native and FFmpeg stages use lossless FFV1 or PCM intermediates so their relative order is retained independently for video and audio. Preserved MP4 metadata includes global title, artist, album, comment, genre, date, creation time, and copyright plus title and language for the selected video and every audio stream. Additional MP4 video streams, subtitles, chapters, attachments, and other metadata classes are not processed.

Omit `--seed` to generate and print one. Existing destinations are replaced by default; use `--protect-output` to refuse replacement. The input file is never overwritten.

Additional JPEG/PNG pixel filters are `brightness`, `contrast`, `saturation`, `hue-rotate`, `posterize`, `invert`, and seeded `row-dropout`. `huffman-glitch` is JPEG-only and defaults to 32 swaps, full intensity, luma AC, run remapping, and preserved amplitude size. Run `cargo run -- list-filters` for codec compatibility. Byte filters such as `byte-swap` operate on PNG scanline payloads and are rejected for JPEG.

Pixel transforms preserve supported basic metadata: JPEG APP1/EXIF, APP2/ICC, and comments; PNG color/profile, resolution, EXIF, text, and timestamp chunks; and WebP ICC, EXIF, and XMP chunks. Pixel-layout-dependent PNG chunks are intentionally not transplanted. AVIF metadata preservation is not yet implemented. Animated WebP and image-sequence AVIF are rejected.

The same seed and native pipeline will produce deterministic stage seeds. FFmpeg-backed stages preserve the resolved plan and parameters, but compressed output is not promised byte-identical across FFmpeg versions.

See [the design](docs/design.md), [specification](docs/spec.md), and [roadmap](docs/roadmap.md) for the implementation contracts and release gates.