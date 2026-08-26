# Specification

## Formats and Filters

The supported formats are JPEG, PNG, WebP, AVIF, WAV, MP3, Ogg Vorbis, Ogg Opus, MP4, and Matroska. Filters are typed values with a stable name, processing domain, target stream, and validated parameters. Arbitrary FFmpeg expressions are not accepted.

Initial filter families are:

- JPEG Huffman tables: deterministic AC symbol swaps within equal amplitude-size classes.
- Encoded bytes: noise, repeat, drop, and swap.
- Images/video frames: channel shift, scanline displacement, pixel sorting, brightness, contrast, saturation, hue rotation, posterization, inversion, and seeded row dropout.
- PCM audio: bounded sample noise.
- FFmpeg audio: high-pass, low-pass, echo, and volume.
- FFmpeg video: hue, equalization, and lag.
- Encoded video packets: bounded H.264, H.265, VP8, VP9, and AV1 payload noise in MP4 samples.

Unsupported format, domain, target, parameter, or ordering combinations fail during preflight. Generic encoded-byte mutation remains excluded for MP3 and MP4; their structure-aware encoded filters expose only protected payload regions.

## CLI

The implemented commands are:

```text
databender list-formats
databender list-filters
databender plan --format FORMAT --seed U64 \
	--filter NAME[:KEY=VALUE,...] [--filter NAME[:KEY=VALUE,...] ...]
databender transform INPUT --output PATH [--seed U64] [--protect-output] \
	[--config PATH --preset NAME] \
	[--video-stream INDEX ...] \
	[--filter NAME[:KEY=VALUE,...] ...]
databender batch INPUT... --output-dir PATH [--layout flat|mirrored] \
	[--root PATH] [--jobs COUNT] [--dry-run] [--json] \
	[--manifest PATH] [--resume PATH] \
	[--config PATH --preset NAME] [--seed U64] \
	[--video-stream INDEX ...] \
	[--protect-output] [--filter NAME[:KEY=VALUE,...] ...]
```

`list-filters` groups names by compatible codec and processing domain. `plan` preserves filter argument order and prints the resolved stages and derived seeds without touching media. Bare filter names use documented defaults. Parameter names are filter-specific; unknown, duplicate, malformed, or out-of-range values are rejected. `transform` executes all image-pixel filters for JPEG and PNG inputs. JPEG additionally supports `huffman-glitch:swaps=COUNT,intensity=FRACTION,target=TARGET,mode=MODE,preserve_size=BOOL`. Defaults are 32 swaps, intensity 1, target `luma-ac`, mode `run-remap`, and preserved size. Targets are `all`, `luma-ac`, and `chroma-ac`; modes are `run-remap` and `symbol-remap`. Intensity quadratically scales swap count and expands candidate selection from rare, nearby run pairs toward broader, higher-impact pairs. Baseline entropy statistics guide symbol selection, while progressive scans use deterministic fallback mutation. Non-interlaced PNG supports byte noise, repeat, drop, and swap over inflated scanline payload bytes; these operations preserve payload length and row filter bytes. Generic payload filters remain rejected for JPEG. Integer PCM WAV supports bounded `audio-noise` and the same length-preserving byte filters over only the `data` chunk. MP3 supports `mp3-main-data-noise:byte_budget=COUNT,start_frame=INDEX,frame_count=COUNT,intensity=FRACTION` plus ordered high-pass, low-pass, echo, and volume effects through FFmpeg. Main-data defaults are an eight-byte budget, frame zero onward, and intensity 0.125; a zero frame count means all remaining frames. Counts are bounded to 1,048,576, intensity to 0 through 1, and CRC-protected frames are excluded. Plans and batch reports expose the upper-bound mutation impact. PCM noise remains rejected for MP3. MP4 and Matroska support all image-pixel and FFmpeg-video filters on one or more repeated `--video-stream` selections, copy unselected video streams, and apply PCM noise and FFmpeg-audio filters to every audio stream. Omitting the selector transforms video stream 0.

Format and filter discovery checks FFmpeg/ffprobe availability and the encoders required by each external adapter. Missing components produce an unavailable status and suppress unusable filter groups. Native codec discovery does not depend on external tools.

JPEG Huffman mutation defaults to `engine=table`, the canonical compatibility mode. It changes DHT symbol mappings globally while leaving entropy bytes and segment geometry unchanged. `engine=coefficient` reconstructs baseline or progressive quantized blocks, including first and refinement scans and restart intervals, then applies deterministic component, source-scan, spectral-band, and intensity targeting. Its additional controls are `scan_start`, `scan_count`, `frequency_start`, and `frequency_end`; a zero scan count selects all scans. Run remapping moves values among nearby zero coefficients, while symbol remapping changes coefficient signs without changing magnitude categories. Output is a valid sequential JPEG using standard DHT segments while preserving source quantization tables, sampling geometry, restart interval, APP1, APP2, and COM metadata. Candidates are independently decoded before publication.

Input format detection uses content signatures, with file extensions used only as diagnostic hints. If no seed is supplied, the CLI generates and reports one. Existing output is replaced by default; `--protect-output` enables no-clobber behavior. In-place editing is not supported.

Configuration files use `version = 1` and named `[presets.NAME]` tables containing a nonempty ordered `filters` array, optional `seed`, and optional `output_policy` of `replace` or `protect`. Files containing presets with encoded mutation or expert graphs must declare the current top-level `plan_version`; missing or unsupported versions are rejected before plan construction. Explicit filters append after preset filters. Explicit seeds override preset seeds, and output protection is enabled if either source requests it. Optional global `[plugins]` and preset `[presets.NAME.plugins]` tables contain `directories` and `disabled` arrays. Relative plugin directories resolve from the configuration file; global, preset, and explicit CLI settings merge in that order.

`expert-audio-graph:<fragment>` and `expert-video-graph:<fragment>` are explicit environment-dependent filter types with fixed stream targets. Fragments are bounded, inspected single-chain FFmpeg graphs. Control/shell characters, labels, multiple chains, protocols, and external-resource filters/options are rejected. Parsed filter availability is checked during preflight, accepted fragments are passed as direct process arguments, and exact resolved graphs are recorded in plans and batch reports.

The MP3 structural parser recognizes MPEG-1/2/2.5 Layer III frame lengths, headers, optional CRCs, side information, reservoir references, leading ID3v2, and trailing ID3v1. Typed mutation selects deterministic bytes without replacement only from unprotected frames' metadata- and framing-disjoint main-data ranges. Candidate structure is reparsed, stream geometry and codec are validated, and the complete stream is decoded before atomic publication.

Batch inputs are explicit files or recursively expanded directories. Regular files without a recognized, case-insensitive media extension are excluded before content-signature detection. Expansion is canonicalized, sorted, and deduplicated before output planning. Flat layout maps each input to its file name and rejects collisions before execution. Mirrored layout requires `--root`, preserves paths relative to that root, and rejects outside inputs. `--jobs` bounds concurrent file transforms. Each canonical input path deterministically derives its own tagged file seed from the base seed; native container processing derives further tagged stream, original stage, and frame seeds. Per-file reports are rendered in sorted input order. Item failures do not cancel other work and produce a nonzero aggregate status. `--dry-run` performs expansion and collision validation without creating outputs, and `--json` emits the complete report as JSON.

`--manifest` atomically writes a versioned report containing the resolved execution fingerprint and encoded-mutation impact estimates. Manifest version 3 records pipeline plan version 1. The fingerprint covers both the explicit plan and Databender versions, filters, video stream selectors, protection policy, canonical mappings, derived seeds, and streamed source bytes. `--resume` rejects unsupported manifest or plan versions before comparing fingerprints or executing work, skips only matching successful items whose outputs still exist, retries failures and missing outputs, and updates the source manifest unless another destination is selected.

The library exposes `TransformRequest::prepare` for content detection, capability validation, and path checks. A resulting `PreparedTransform` exposes the resolved plan and accepts codec writer and validator closures through `publish_with`, or path-based closures through `publish_path_with` for external encoders. Publication uses a same-directory temporary candidate and atomically replaces the destination by default. `with_output_protection(true)` enables atomic no-clobber publication.

`ToolRunner` provides shell-free `ffmpeg` and `ffprobe` invocation with null standard input, concurrent bounded output capture, a configurable timeout, and structured errors for launch failure, timeout, and nonzero exit. The default timeout is 120 seconds and the default capture limit is 64 KiB per stream.

FFmpeg audio graphs are compiled exclusively from typed high-pass, low-pass, echo, and volume filters. Video graphs are compiled exclusively from typed hue, equalization, and lag filters; caller-supplied graph expressions are never interpolated. ffprobe output is requested as JSON and parsed into typed audio, video, metadata, and Matroska auxiliary information; video probes count decoded frames. Validation preserves ordered video dimensions, average frame rates, frame counts, and allowlisted metadata plus every audio stream's index, sample rate, and channel count. MP3 execution preserves stage order across native main-data mutation and FFmpeg re-encoding, maps source metadata for encoded stages, requires the final primary stream to report the MP3 codec, reparses its frame structure, and fully decodes the candidate before publication. Ogg packet mutation protects Vorbis and Opus header packets, follows lacing across pages, preserves logical-stream sequence numbers, repairs touched page CRCs, and reparses and fully decodes the final candidate. Its `max_decode_errors` limit counts non-empty FFmpeg error-level diagnostic lines from that decode, rejects truncated diagnostics, defaults to zero, and resolves multiple packet-filter limits to the strictest value. MP4 and Matroska execution map selected video streams independently and all audio streams. Per-target order is preserved across native and FFmpeg domains through FFV1 video and 16-bit PCM WAV intermediates. Image filters receive RGBA8 frames and a deterministic frame-derived seed; PCM noise is applied independently to every audio stream. MP4 encodes filtered video as MPEG-4 video and audio as AAC; Matroska uses FFV1 and FLAC. Unfiltered streams are copied in source order. Matroska also copies subtitle and attachment streams plus chapters, preserving and validating auxiliary codecs, chapter IDs and timing, and supported title, language, filename, and MIME-type tags. Validation requires content detection, exact allowlisted metadata, unchanged stream geometry and timing, complete decoding of every video and audio stream, and exact Matroska auxiliary topology. Video-only input is supported when the pipeline contains no audio effects.

Supported metadata is preserved and validated exactly. JPEG preservation covers APP1, APP2, and COM segments. PNG preservation covers color/profile (`cHRM`, `gAMA`, `iCCP`, `sRGB`), physical resolution, EXIF, text, and timestamp chunks. WebP preservation covers ICC, EXIF, and XMP chunks. AVIF preservation covers Exif and XMP metadata items, ICC/CICP color properties, and rotation/mirroring properties associated with the primary image. MP4 preservation covers the allowlisted global and selected-stream tags described above. Metadata that depends on the original pixel representation is dropped rather than copied into a potentially incompatible re-encode. WAV transforms preserve the RIFF layout and every byte outside the `data` chunk.

## Output Contract

Output is structure-aware and best effort. If an adapter or independent decoder detects an invalid candidate, the operation fails before publication. Supported metadata classes are preserved byte-for-byte and validated before publication; unsupported classes are intentionally dropped as documented by each adapter.
