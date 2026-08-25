# Design

Databender is one Cargo package exposing a reusable library and a thin CLI. The library owns media types, filter specifications, capability validation, pipeline planning, execution, and errors. The CLI only parses user input, invokes the library, and renders results.

## Pipeline Model

Filters belong to explicit processing domains:

- `JpegHuffmanTables` mutates AC symbols in JPEG DHT segments.
- `EncodedPayload` operates only on adapter-supplied safe payload regions.
- `ImagePixels` operates on decoded image or video frames.
- `PcmAudio` operates on decoded, frame-aligned samples.
- `FfmpegAudio` and `FfmpegVideo` compile typed effects into allowlisted FFmpeg graphs.

Each codec advertises supported domains. Preflight validation checks the entire ordered filter list and groups adjacent filters with the same target and domain into stages. A stable mixing function derives a deterministic seed for each stage.

## Execution Model

JPEG, PNG, and WAV use native Rust decoders plus narrow container scanners. MP3 and MP4 use installed `ffmpeg` and `ffprobe` subprocesses. `ToolRunner` constructs commands as argument arrays without a shell, nulls standard input, passes FFmpeg its supported `-nostdin` flag, drains standard output and error concurrently, and retains only configurable bounded tails. It reports launch errors and nonzero statuses distinctly. A configurable deadline defaults to 120 seconds; expired children are killed and reaped before control returns.

`compile_audio_graph` maps only typed `AudioEffect` values to allowlisted FFmpeg filters and preserves stage order. High-pass and low-pass compile to frequency-bounded filters, echo uses fixed input/output gains plus typed delay and decay, and volume accepts only the parser's bounded gain. `compile_video_graph` similarly maps hue, equalization, and lag to bounded `hue`, `eq`, and `tmix` filters. `ToolRunner` requests structured JSON for primary audio and video streams. Validation rejects missing or malformed streams, changes to sample rate or channel count, and changes to video dimensions. The MP3 adapter maps the primary audio stream and source metadata, applies the compiled graph, explicitly encodes MP3 with `libmp3lame`, validates its stream geometry and codec, and performs a complete decode to FFmpeg's null muxer before publication.

The MP4 adapter partitions ordered stages by target stream and maps `0:v:0` plus every `0:a` stream. FFmpeg video stages produce lossless FFV1 intermediates. Image-pixel stages decode RGBA8, apply native filters one frame at a time with a frame-derived stage seed, and return to FFV1. Each audio stream independently traverses its ordered stages as 16-bit PCM WAV: FFmpeg audio effects produce a fresh intermediate, while PCM stages apply native bounded noise to decoded samples. The final fast-start mux encodes processed video as MPEG-4 video and processed audio as AAC; targets with no stages are copied directly from the source. Before processing, the adapter snapshots an allowlist of global title, artist, album, comment, genre, date, creation time, and copyright tags plus selected-stream title and language. Final validation requires an MP4 signature, unchanged metadata, video dimensions, audio stream count and per-stream geometry, then decodes the selected video and every audio stream completely to FFmpeg's null muxer. Video-only MP4 input is accepted when no audio effect is requested. Additional video streams, subtitles, chapters, attachments, and unlisted metadata are out of scope for v0.3.

The image executor accepts ordered JPEG Huffman/pixel stages and PNG pixel/payload stages. For baseline sequential JPEGs, Huffman stages parse SOF0, DHT, DRI, and SOS structures, decode entropy symbols MCU-by-MCU without reconstructing pixels, and count AC symbol use per table. Actual SOS component assignments classify tables as luma or chroma. Intensity scales swap count quadratically. Candidate pairs are ranked by combined observed use; low intensity draws from the lowest-impact fraction and limits run-distance changes, while higher intensity progressively broadens the pool. The default target is luma AC to avoid immediate global color scrambling. Run remapping changes zero-run meanings while preserving amplitude size; symbol remapping can optionally cross size classes for less stable corruption. Segment lengths, code counts, and entropy data remain unchanged. Progressive or unsupported scan layouts use deterministic DHT fallback mutation rather than incomplete coefficient analysis. Pixel stages decode to RGBA8, apply adjacent filters, and re-encode to the detected input format. PNG payload stages validate all chunk bounds and CRCs, concatenate and inflate `IDAT`, preserve each non-interlaced row's filter byte, mutate only the fixed-length row payload, recompress it, and replace the original `IDAT` chunks with one correctly framed chunk. This permits transitions between domains without publishing an invalid compressed stream.

The WAV executor scans RIFF chunks with padding, validates integer PCM geometry and whole-frame alignment, and supports 8-, 16-, 24-, and 32-bit samples. PCM noise is applied per sample with deterministic probability, bounded amplitude, and saturation. Encoded-payload filters operate only on the fixed-length `data` chunk. Candidate validation reparses the output and requires the original format, data range, and every byte outside that range to remain exact.

Byte repeat copies selected bytes forward. Byte drop shifts subsequent bytes left and zero-fills the tail. Noise and swap are also length-preserving. These semantics keep scanline framing stable. Byte-only PNG pipelines preserve all non-`IDAT` chunks.

Before image execution, the codec snapshots supported metadata. Every pixel re-encode transplants that snapshot, and candidate validation extracts it again for a byte-for-byte comparison. JPEG preserves APP1 (including EXIF/XMP), APP2 (including segmented ICC profiles), and COM segments in source order. PNG preserves `cHRM`, `gAMA`, `iCCP`, `sRGB`, `pHYs`, `eXIf`, text, and timestamp chunks. Chunks tied to the original pixel layout, such as `tRNS`, `hIST`, and `bKGD`, are not transplanted. JPEG drops alpha during encoding and rejects generic payload stages during preflight.

## File Safety

`TransformRequest::prepare` canonicalizes paths, detects the input format from its content signature, and validates a complete plan before creating output. It always rejects identical input/output paths. Existing destinations are replaced by default; callers can enable output protection for race-safe no-clobber publication. `PreparedTransform::publish_with` writes candidates to a temporary file beside the destination, flushes and synchronizes them, runs adapter validation, and atomically persists them. `publish_path_with` provides the same lifecycle for external encoders that require a destination path. Failed runs leave existing files untouched.

## Determinism

The CLI reports generated seeds, while the library requires an explicit seed. Native stages using the same input, options, seed, and Databender version promise byte-identical output. FFmpeg-backed output may vary across FFmpeg builds; its reproducibility contract covers the resolved pipeline and parameters instead.
