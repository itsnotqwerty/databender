# Migrating to v0.6

## Encoded Mutation Plans

Presets containing MP3, Ogg, or MP4 encoded mutation must declare `plan_version = 1`. Batch manifests use schema version 3 and record this plan version. Regenerate older manifests; they cannot be resumed.

MP4 adds packet-only mutation through:

```bash
databender transform input.mp4 --output output.mp4 --seed 42 \
  --video-stream 0 \
  --filter video-packet-noise:byte_budget=8,start_packet=0,packet_count=0,frame_type=all,intensity=0.125,max_frame_loss=0
```

A zero packet count means the selected packet onward. Packet stages cannot be mixed with decoded image, video, PCM, or FFmpeg stages in one pipeline. Matroska resolves ffprobe packets to exact unlaced EBML block payloads by size and SHA-256; selected packets stored with lacing fail closed and should use decoded filters instead.

MP4 packet candidates preserve file length and container bookkeeping through equal-length writes. Execution now fails if packet ranges overlap, omit positions or SHA-256 hashes, or if bytes at a probed range do not match the hash. Existing workflows that relied on ambiguous ffprobe positions must move to the typed MP4 executor.

## Expert Graphs

Caller-supplied FFmpeg expressions must use `expert-audio-graph:<fragment>` or `expert-video-graph:<fragment>`. They are environment-dependent and require explicit stream targets. Labels, multiple chains, protocols, shell/control characters, and options or filters that access external resources are rejected. Presets using expert graphs also require `plan_version = 1`.

## Plugins

Plugin manifests and the WebAssembly ABI are independently versioned; v0.6 requires manifest version 1 and ABI version 1. Bundles contain adjacent `<name>.plugin.json` and `<name>.wasm` files. Modules must be import-free and export `memory`, `databender_alloc(i32) -> i32`, and `databender_run(i32, i32) -> i64`.

Plugin directories are explicit. Move implicit or globally scanned plugins into configured `[plugins].directories`, preset-specific directories, or repeated `--plugin-dir` arguments. Disabled IDs merge across configuration and CLI. Run `list-plugins` after migration to verify compatibility and provenance without executing modules.

The v1 runtime validates media topology and applies memory, message, instance, table, module-size, and fuel limits. Plugins that imported WASI or host functions must be rebuilt as pure computation over typed invocation bytes.

## Image and Container Validation

Still-image decoding now rejects either dimension above 16,384 pixels and limits decoder-tracked allocation to 512 MiB. AVIF EXIF is preserved for still images; unsupported AVIF ICC/XMP/color-property reconstruction remains outside the current guarantee.

Encoded MP3, Ogg, and MP4 candidates are reparsed and fully decoded before atomic publication. MP4 additionally verifies codec, dimensions, frame rate, metadata, audio topology, and the configured selected-stream frame-loss bound. These checks can reject corruptions accepted by earlier versions; lower mutation intensity or damage limits rather than bypassing validation.

## Determinism and Golden Outputs

Native output is deterministic for a fixed Databender version, plugin version, seed, input bytes/path identity, selectors, and plan. FFmpeg and expert-graph output remains environment-dependent. Packet seeds add a tagged packet identity below stage and video-stream identities, so encoded-video golden bytes are new in v0.6.

Recreate byte-level golden files when upgrading. Compare decoded geometry, timing, metadata, topology, and documented mutation bounds when testing across FFmpeg versions or operating systems.
