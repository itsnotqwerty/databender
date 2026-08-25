# Migrating to v0.5

## Video stream selection

MP4 and Matroska transforms still target video stream 0 when no selector is supplied. Use repeated zero-based selectors to transform specific streams while copying the others in source order:

```bash
databender transform input.mkv --output output.mkv \
  --video-stream 0 --video-stream 2 --filter invert
```

Batch accepts the same repeated option. Video stream selectors are part of the resumable manifest fingerprint, so a manifest created with different selectors is rejected before work begins.

## Deterministic seeds

Native container transforms now derive tagged seeds hierarchically from file, stream, original stage, and frame identities. Output remains repeatable for the same Databender version, root seed, input path, and pipeline, and no longer depends on worker scheduling.

This changes seeded bytes relative to earlier releases. Recreate golden outputs when upgrading instead of expecting byte-for-byte compatibility with pre-v0.5 results.

## Output validation

MP4 and Matroska publication now verifies every ordered video stream's dimensions, average frame rate, decoded frame count, and allowlisted metadata. Every video and audio stream is completely decoded before the temporary candidate atomically replaces the destination.

Malformed containers, duplicate selectors, out-of-range selectors, topology changes, timing changes, metadata changes, and decode failures leave the destination unpublished.

## Pipeline plan compatibility

Resolved plans now print pipeline plan version `1`. Presets containing encoded mutation or expert FFmpeg graphs must declare `plan_version = 1` beside their configuration `version`; ordinary pixel and PCM presets remain compatible without it.

Batch manifests use schema version `3` and record the pipeline plan version independently. Older manifests cannot be resumed and should be regenerated with `batch --dry-run --json` or a normal batch execution. Resume rejects unsupported manifest and plan versions before fingerprint comparison or output processing.