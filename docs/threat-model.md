# Threat Model

## Scope and Trust Boundaries

Databender processes attacker-controlled media, configuration, plugin manifests, and WebAssembly modules. The Rust process, its configured output directory, and explicitly selected external executables are trusted. FFmpeg, ffprobe, avifenc, the operating system, and the filesystem are outside Databender's memory-safety boundary and must be maintained as trusted dependencies.

The CLI never invokes a shell. External tool paths come from `PATH` or the explicit `DATABENDER_FFMPEG`, `DATABENDER_FFPROBE`, and `DATABENDER_AVIFENC` variables; anyone who can alter those values can choose code executed with the user's privileges. Input media and plugin files must not control executable paths, process arguments outside typed values, environment variables, or working directories.

## Security Goals

Databender aims to:

- reject malformed structure before out-of-bounds reads, writes, or unbounded allocation;
- mutate only regions declared safe by a codec adapter;
- preserve the source and existing destination until a candidate passes validation;
- prevent plugins and expert graphs from acquiring implicit filesystem, network, process, or shell access;
- bound child-process time and captured diagnostics, plugin memory/fuel/messages, image dimensions, and decoder-tracked image allocation;
- make unsupported formats, codecs, graph features, and ambiguous packet offsets fail closed;
- keep deterministic plans, seeds, plugin versions, and environment-dependent behavior visible in reports.

Databender does not claim that intentionally corrupted output is safe for every decoder. Candidates should still be treated as untrusted media when opened by other software.

## Threats and Mitigations

### Malformed and adversarial media

Native scanners use checked offsets and lengths and reject truncation, overlap, invalid framing, unsupported construction methods, and inconsistent counts. Still-image decoding rejects dimensions above 16,384 and limits tracked allocations to 512 MiB. FFmpeg-backed outputs are reprobed and fully decoded before publication. Parser and mutation tests include malformed, adversarial, truncation, and generated-property cases.

Residual risk: native parser defects and vulnerabilities in image, FFmpeg, or libavif dependencies. Keep dependencies patched and process hostile corpora in an operating-system sandbox when stronger isolation is required.

### Encoded-payload corruption

MP3, Ogg, MP4, and Matroska mutation adapters expose codec-specific mutable regions. Protected headers, parameter sets, side information, packet lacing, checksums, and container bookkeeping are not generic byte targets. MP4 packet writes are length-preserving, non-overlapping, and accepted only when bytes at each ffprobe `pos`/`size` range match ffprobe's SHA-256 packet hash. Matroska uses checked EBML element bounds and accepts only unlaced block payloads matched by size and SHA-256; ambiguous or unmatched packets fail closed.

Residual risk: syntactically decodable corruption may trigger bugs in downstream decoders. Damage limits and full decode validation reduce, but cannot eliminate, that risk.

### Output replacement and path handling

Candidates are created beside the destination, validated, flushed, and atomically persisted. `--protect-output` uses no-clobber publication. Inputs are never edited in place. Batch expansion canonicalizes, sorts, and deduplicates paths and rejects flat-layout collisions and mirrored paths outside the declared root.

Residual risk: local filesystems determine atomicity and durability guarantees. Network filesystems, concurrent directory replacement, hostile symlinks outside the canonicalized planning window, and exhausted storage can weaken them. Use a trusted local output directory for hostile workloads.

### External tools and expert graphs

Commands use direct argument arrays, null standard input, concurrent bounded output capture, cancellation, and a 120-second default deadline. Expert graphs are explicit environment-dependent types; syntax inspection rejects labels, multiple chains, protocols, shell/control characters, and external-resource filters or options. Typed filter availability is checked before execution.

Residual risk: FFmpeg filter behavior and vulnerabilities vary by build. Tool binaries and their search path are trusted configuration. Run untrusted media under a restricted OS account or container when external-tool compromise is in scope.

### WebAssembly plugins

Discovery validates manifests, duplicate IDs, module size, imports, exports, and ABI versions without execution. Runtime modules are import-free and receive no WASI, filesystem, environment, network, clock, random, process, or host callback capability. Wasmtime limits module bytes, linear memory, input/output messages, instances, tables, memories, and fuel. Cancellation interrupts execution by epoch. Returned invocation IDs, events, geometry, formats, and region topology are validated.

Residual risk: Wasmtime defects and denial of service below configured limits. Plugin authenticity is outside ABI v1; distributors should publish checksums or signatures and users should configure only trusted plugin directories.

### Resource exhaustion

Batch jobs, native frame workers, subprocess output, subprocess duration, plugin resources, image dimensions, and tracked image allocation are bounded. Audio/video duration and aggregate temporary-disk use are not globally capped. Operators should use `--jobs 1`, filesystem quotas, process memory limits, and representative dry runs for large or hostile inputs. See [Operational Limits](operations.md).

## Non-Goals

Databender does not provide malware scanning, media confidentiality, encrypted storage, plugin signature verification, a hardened FFmpeg sandbox, guaranteed output compatibility with every independent decoder, or atomicity stronger than the destination filesystem. It does not allow arbitrary byte mutation where a structure-safe adapter is unavailable.

## Release Checks

Every release runs formatting, all-target tests, and strict Clippy on Linux, macOS, and Windows with the pinned Rust and FFmpeg versions in CI. Security-relevant changes require focused malformed-input or limit tests, documentation updates, and a version bump when plan, manifest, or plugin ABI behavior changes.
