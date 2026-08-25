# Roadmap

## Foundation

- [x] Bootstrap one library and CLI package.
- [x] Define formats, streams, typed filter domains, and errors.
- [x] Validate capabilities and build deterministic ordered stages.
- [x] Add format/filter discovery and pipeline preflight commands.
- [x] Parse and validate typed filter parameters.
- [x] Add transform requests, content probing, and atomic output sessions.

## v0.1: JPEG and PNG

- [x] Decode and encode images with native Rust tooling.
- [x] Scan and rebuild safe PNG `IDAT` scanline payloads.
- [x] Implement byte noise, repeat, drop, and swap with PNG container repair.
- [x] Implement channel shift, scanline displacement, and pixel sorting.
- [x] Add deterministic, frequency-aware JPEG Huffman-table mutation with targeted controls.
- [x] Preserve supported basic metadata and validate every candidate.
- [x] Add generated deterministic fixtures, integration tests, and CLI transformation.

## v0.2: WAV and FFmpeg Foundation

- [x] Parse RIFF/WAV and represent aligned PCM samples.
- [x] Implement bounded audio noise and safe payload mutations.
- [x] Add timeout-aware `ffmpeg` and `ffprobe` subprocess handling.
- [x] Compile typed audio effects and validate outputs with `ffprobe`.

## v0.3: MP3 and MP4

- [x] Add FFmpeg-backed MP3 decode, filtering, and encode.
- [x] Process one MP4 video stream and all included audio streams.
- [x] Apply image filters to video frames and PCM filters to audio.
- [x] Remux, preserve basic metadata, and fully decode-test outputs.

## v0.4: More Codecs and Repeatable Workflows

### Format Foundation

- [x] Extend content detection, typed capabilities, CLI discovery, and output validation for WebP, AVIF, Ogg, and Matroska.
- [x] Represent container and stream codec separately so Ogg and Matroska routing is based on probed stream codecs rather than filename extensions.
- [ ] Add generated fixtures and malformed-input coverage for every new container and supported codec combination.

### WebP and AVIF Images

- [x] Apply the existing image-pixel filters to still WebP and AVIF images, preserving dimensions and alpha.
- [ ] Preserve and validate supported EXIF, XMP, ICC, orientation, and color metadata for both formats.
- [x] Reject animated WebP and image-sequence AVIF before processing until frame timing and animation metadata have explicit contracts.

### Ogg Audio

- [x] Support Ogg Vorbis and Ogg Opus inputs with PCM and typed FFmpeg audio stages in pipeline order.
- [x] Preserve the input audio codec when encoding, retain Vorbis comments, and validate every logical audio stream.
- [x] Fully decode-test Ogg candidates before atomic publication and reject unsupported Ogg codecs with a precise error.

### Matroska Audio and Video

- [x] Reuse the staged MP4 executor for one selected Matroska video stream and all audio streams without assuming MP4 codecs.
- [x] Preserve compatible source codecs for untouched streams and choose documented Matroska-safe codecs for transformed streams.
- [ ] Preserve and validate global and stream metadata, chapters, subtitle streams, and attachments during remux.
- [ ] Fully decode-test transformed audio/video streams and verify copied auxiliary streams before atomic publication.

### Presets, Batch, and Parallelism

- [ ] Add versioned TOML configuration and named presets for ordered filters, parameters, seeds, and output policy.
- [ ] Add batch transformation with explicit input expansion, mirrored or flat output layouts, collision checks, and per-file results.
- [ ] Add bounded parallel file processing and bounded parallel video-frame filtering while preserving deterministic per-item and per-frame seeds.
- [ ] Add dry-run planning, machine-readable JSON reports, aggregate failure status, and resumable batch manifests.

### Release Gates

- [ ] Keep format/filter discovery honest for optional native and FFmpeg codec availability.
- [ ] Add end-to-end CLI tests for each new format, preset loading, partial batch failure, deterministic replay, metadata retention, and output protection.
- [ ] Benchmark memory, temporary-disk use, and throughput for large AVIF images, long Ogg audio, and multi-stream Matroska files; document operational limits.
- [ ] Update the specification, design, examples, and support matrix, then complete formatting, tests, and strict linting across all targets.

## v0.5: Interactive and Portable Workflows

### Shared Application Layer

- [ ] Extract a presentation-independent application service for probing media, editing plans, resolving presets, running jobs, and reporting typed progress events.
- [ ] Add cooperative cancellation across native stages, FFmpeg processes, frame workers, and batch queues without publishing partial candidates.
- [ ] Persist bounded job history, diagnostics, resolved seeds, and output paths in a versioned local state format.
- [ ] Keep CLI, JSON reporting, and TUI behavior aligned through shared commands and result types rather than separate execution paths.

### Terminal User Interface

- [ ] Build a keyboard-driven TUI for browsing inputs, inspecting detected streams and metadata, and selecting output locations.
- [ ] Add an ordered pipeline editor with codec-aware filter discovery, typed parameter controls, preset loading, and immediate preflight errors.
- [ ] Add still-image and sampled-video previews plus audio waveform summaries generated through the same decoded media paths used for transforms.
- [ ] Add queue controls for starting, pausing intake, cancelling, retrying, and resuming batch jobs with per-file progress and bounded live logs.
- [ ] Support accessible color themes, no-color terminals, narrow layouts, mouse-free operation, and documented key bindings.

### Animated Images and Multiple Video Streams

- [ ] Process animated WebP and image-sequence AVIF frame-by-frame while preserving frame order, duration, loop count, alpha, and supported metadata.
- [ ] Add explicit video-stream selectors for MP4 and Matroska, with one or more selected streams transformed and unselected streams copied.
- [ ] Derive deterministic seeds from file, stream, stage, and frame identities so worker scheduling cannot change native output.
- [ ] Validate frame counts, timing, stream ordering, metadata, and complete decoding before atomic publication.

### macOS and Windows

- [ ] Support macOS and Windows path semantics, atomic replacement behavior, temporary files, process termination, and terminal capability detection.
- [ ] Discover FFmpeg and ffprobe through explicit configuration and `PATH`, and report unavailable encoders or filters during preflight.
- [ ] Add platform-specific integration fixtures and CI jobs for Linux, macOS, and Windows using pinned Rust and FFmpeg versions.
- [ ] Document platform differences in codec availability, filesystem guarantees, terminal behavior, and reproducibility.

### Release Gates

- [ ] Add application-service tests proving CLI/TUI plan parity, cancellation cleanup, resumable queues, and deterministic concurrent execution.
- [ ] Add terminal snapshot and interaction tests across supported dimensions, color modes, keyboard flows, and failure states.
- [ ] Add end-to-end animated-image and multi-video-stream tests covering timing, metadata, stream selection, and malformed inputs.
- [ ] Run usability sessions for pipeline editing and batch recovery, publish migration notes, and complete formatting, tests, strict linting, and cross-platform CI.

## v0.6: Extensibility and Encoded Databending

### Plugin Runtime and SDK

- [ ] Define a versioned plugin manifest and typed ABI for filter metadata, parameters, supported domains, deterministic seeds, progress, cancellation, and structured errors.
- [ ] Run third-party filters as sandboxed WebAssembly components with explicit memory, CPU, temporary-storage, and host-capability limits.
- [ ] Support image-frame, PCM-audio, and structure-aware encoded-payload plugin interfaces without exposing arbitrary filesystem, network, or process access.
- [ ] Add plugin discovery, compatibility checks, enable/disable controls, provenance display, and configuration to the library, CLI, TUI, and preset schema.
- [ ] Publish an SDK, example plugins, conformance tests, ABI migration policy, and packaging guidance.

### Expert FFmpeg Graphs

- [ ] Add an explicit expert-mode filter type for caller-supplied FFmpeg audio and video graph fragments; keep typed allowlisted filters as the default interface.
- [ ] Parse and inspect graph syntax before execution, reject shell interpolation and disallowed protocols or external resource access, and pass accepted graphs only as direct process arguments.
- [ ] Require explicit stream targets, record the resolved graph in plans and reports, and surface FFmpeg filter availability during preflight.
- [ ] Mark expert pipelines as environment-dependent, preserve timeout and cancellation guarantees, and retain geometry, metadata, full-decode, and atomic-publication validation.

### Encoded Audio Databending

- [ ] Parse MP3 frame headers, side information, CRC fields, and safe main-data regions so bounded mutations never overwrite framing or metadata.
- [ ] Parse Ogg pages, lacing values, logical streams, and codec packets; mutate eligible Vorbis or Opus packet payloads and rebuild page CRCs and sequence continuity.
- [ ] Add typed encoded-audio mutations with byte budgets, packet/frame targeting, intensity controls, deterministic seeds, and dry-run impact estimates.
- [ ] Validate repaired containers, preserve stream geometry and metadata, fully decode candidates, and discard mutations that violate configured damage limits.

### Encoded Video Databending

- [ ] Demux MP4 and Matroska samples into codec-aware packet records while preserving timestamps, keyframe flags, codec configuration, and auxiliary streams.
- [ ] Add initial structure-aware mutation adapters for documented H.264/H.265 NAL-unit payload classes and VP8/VP9/AV1 frame payload classes supported by the project toolchain.
- [ ] Protect parameter sets, container indexes, sample tables, timestamps, and mandatory frame headers; rebuild affected container bookkeeping during remux.
- [ ] Add typed stream, packet, frame-type, byte-budget, and intensity controls with deterministic mutation selection and dry-run impact estimates.
- [ ] Validate stream topology, timing, metadata, decoder progress, and configurable tolerated frame loss before atomic publication.

### Progressive JPEG Reconstruction

- [ ] Decode baseline and progressive JPEG scans into quantized coefficient blocks, including DC/AC first scans, refinement scans, restart intervals, and component/table selection.
- [ ] Apply Huffman glitches to reconstructed coefficient and symbol data with explicit scan, component, frequency-band, and impact controls.
- [ ] Re-encode valid entropy scans and DHT segments while preserving quantization tables, sampling geometry, restart behavior, and supported metadata.
- [ ] Keep the existing deterministic table-level fallback available as a compatibility mode and document how its output differs from coefficient reconstruction.

### Safety, Compatibility, and Release Gates

- [ ] Add corpus, property, mutation, and coverage-guided fuzz tests for every new parser, packet repair path, progressive scan decoder, and plugin ABI boundary.
- [ ] Add deterministic golden tests and independent decoder checks across supported codecs, containers, operating systems, FFmpeg versions, and plugin runtime versions.
- [ ] Add resource-exhaustion, cancellation, malicious-plugin, malformed-graph, decompression-bomb, and adversarial-media tests with documented limits.
- [ ] Version encoded-mutation and expert-graph plans so presets and batch manifests fail clearly when behavior or codec support changes.
- [ ] Benchmark native reconstruction, sandbox overhead, packet mutation, remuxing, and validation; publish stability tiers and operational guidance for each advanced feature.
- [ ] Update the specification, architecture, SDK documentation, threat model, support matrix, examples, and migration notes before completing all cross-platform release gates.

## Deferred

A graphical interface remains deferred.
