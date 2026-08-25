# Specification

## Formats and Filters

The target formats are JPEG, PNG, WAV, MP3, and MP4. Filters are typed values with a stable name, processing domain, target stream, and validated parameters. Arbitrary FFmpeg expressions are not accepted.

Initial filter families are:

- JPEG Huffman tables: deterministic AC symbol swaps within equal amplitude-size classes.
- Encoded bytes: noise, repeat, drop, and swap.
- Images/video frames: channel shift, scanline displacement, pixel sorting, brightness, contrast, saturation, hue rotation, posterization, inversion, and seeded row dropout.
- PCM audio: bounded sample noise.
- FFmpeg audio: high-pass, low-pass, echo, and volume.
- FFmpeg video: hue, equalization, and lag.

Unsupported format, domain, target, parameter, or ordering combinations fail during preflight. Encoded-byte mutation of MP3 and MP4 is excluded until a structure-safe adapter exists.

## CLI

The implemented commands are:

```text
databender list-formats
databender list-filters
databender plan --format FORMAT --seed U64 \
	--filter NAME[:KEY=VALUE,...] [--filter NAME[:KEY=VALUE,...] ...]
databender transform INPUT --output PATH [--seed U64] [--protect-output] \
	--filter NAME[:KEY=VALUE,...] [--filter NAME[:KEY=VALUE,...] ...]
```

`list-filters` groups names by compatible codec and processing domain. `plan` preserves filter argument order and prints the resolved stages and derived seeds without touching media. Bare filter names use documented defaults. Parameter names are filter-specific; unknown, duplicate, malformed, or out-of-range values are rejected. `transform` executes all image-pixel filters for JPEG and PNG inputs. JPEG additionally supports `huffman-glitch:swaps=COUNT,intensity=FRACTION,target=TARGET,mode=MODE,preserve_size=BOOL`. Defaults are 32 swaps, intensity 1, target `luma-ac`, mode `run-remap`, and preserved size. Targets are `all`, `luma-ac`, and `chroma-ac`; modes are `run-remap` and `symbol-remap`. Intensity quadratically scales swap count and expands candidate selection from rare, nearby run pairs toward broader, higher-impact pairs. Baseline entropy statistics guide symbol selection, while progressive scans use deterministic fallback mutation. Non-interlaced PNG supports byte noise, repeat, drop, and swap over inflated scanline payload bytes; these operations preserve payload length and row filter bytes. Generic payload filters remain rejected for JPEG. Integer PCM WAV supports bounded `audio-noise` and the same length-preserving byte filters over only the `data` chunk. MP3 supports ordered high-pass, low-pass, echo, and volume effects through FFmpeg; PCM noise is rejected during preflight. MP4 supports all image-pixel and FFmpeg-video filters on its selected video stream, plus PCM noise and FFmpeg-audio filters on every audio stream.

Input format detection uses content signatures, with file extensions used only as diagnostic hints. If no seed is supplied, the CLI generates and reports one. Existing output is replaced by default; `--protect-output` enables no-clobber behavior. In-place editing is not supported.

The library exposes `TransformRequest::prepare` for content detection, capability validation, and path checks. A resulting `PreparedTransform` exposes the resolved plan and accepts codec writer and validator closures through `publish_with`, or path-based closures through `publish_path_with` for external encoders. Publication uses a same-directory temporary candidate and atomically replaces the destination by default. `with_output_protection(true)` enables atomic no-clobber publication.

`ToolRunner` provides shell-free `ffmpeg` and `ffprobe` invocation with null standard input, concurrent bounded output capture, a configurable timeout, and structured errors for launch failure, timeout, and nonzero exit. The default timeout is 120 seconds and the default capture limit is 64 KiB per stream.

FFmpeg audio graphs are compiled exclusively from typed high-pass, low-pass, echo, and volume filters. Video graphs are compiled exclusively from typed hue, equalization, and lag filters; caller-supplied graph expressions are never interpolated. ffprobe output is requested as JSON and parsed into typed audio, video, and basic metadata information. Validation preserves video width and height plus every audio stream's index, sample rate, and channel count. MP3 execution maps source metadata, encodes with `libmp3lame`, requires the resulting primary stream to report the MP3 codec, and fully decodes the candidate before publication. MP4 execution maps one primary video stream and all audio streams. Per-target order is preserved across native and FFmpeg domains through FFV1 video and 16-bit PCM WAV intermediates. Image filters receive RGBA8 frames and a deterministic frame-derived seed; PCM noise is applied independently to every audio stream. Filtered video is encoded as MPEG-4 video, filtered audio streams as AAC, and unfiltered streams are copied into a fast-start MP4. Global title, artist, album, comment, genre, date, creation time, and copyright plus selected-stream title and language are preserved exactly. Validation requires MP4 content detection, exact allowlisted metadata, unchanged stream geometry, and complete decoding of the selected video and all audio streams. Video-only input is supported when the pipeline contains no audio effects.

Supported metadata is preserved and validated exactly. JPEG preservation covers APP1, APP2, and COM segments. PNG preservation covers color/profile (`cHRM`, `gAMA`, `iCCP`, `sRGB`), physical resolution, EXIF, text, and timestamp chunks. MP4 preservation covers the allowlisted global and selected-stream tags described above. Metadata that depends on the original pixel representation is dropped rather than copied into a potentially incompatible re-encode. WAV transforms preserve the RIFF layout and every byte outside the `data` chunk.

## Output Contract

Output is structure-aware and best effort. If an adapter or independent decoder detects an invalid candidate, the operation fails before publication. Supported metadata classes are preserved byte-for-byte and validated before publication; unsupported classes are intentionally dropped as documented by each adapter.
