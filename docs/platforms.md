# Platform Support

Databender targets Linux, macOS, and Windows with Rust 1.89.0 and FFmpeg 7.1 in continuous integration. Paths remain `Path`/`PathBuf` values internally; content detection does not depend on filename extensions, separators, or UTF-8 conversion. Batch seeds and fingerprints hash the exact platform path encoding, so distinct non-UTF-8 Unix paths and native Windows paths retain distinct identities.

Prepared outputs are created beside their destination and atomically persisted only after validation. Replacement and no-clobber publication use `tempfile`'s platform implementation. The destination directory therefore determines filesystem atomicity; network filesystems and cross-device moves may provide weaker guarantees than local APFS, NTFS, ext4, or similar filesystems.

FFmpeg, ffprobe, and the optional libavif `avifenc` tool are found through `PATH` unless `DATABENDER_FFMPEG`, `DATABENDER_FFPROBE`, and `DATABENDER_AVIFENC` provide explicit executable paths. `avifenc` is required only when an AVIF sequence contains an auxiliary alpha track. Child processes receive arguments directly without a shell. Timeout and cancellation use `Child::kill` followed by `wait`; Unix-only shell harnesses are excluded from Windows tests while the production process path is exercised by codec integration tests on every platform.

The TUI requires interactive standard input and output and reports a preflight error before enabling raw mode when either stream is redirected. It uses Crossterm for terminal sizing and supports standard, high-contrast, and monochrome themes. `NO_COLOR`, `--no-color`, or a `dumb` terminal selects monochrome output. Terminal dimensions change the pane arrangement; all operations are available without a mouse.

Codec and filter availability can differ between FFmpeg distributions. Discovery and pipeline preflight verify the configured tools, required encoders, and typed filters. Native transforms are deterministic for a fixed version and seed. FFmpeg-compressed bytes are environment-dependent and may vary across operating systems or FFmpeg builds even when decoded geometry and metadata remain equivalent.

## Advanced Feature Support

| Feature | Linux | macOS | Windows | Stability |
| --- | --- | --- | --- | --- |
| Native image, WAV, and structural parsing | CI gated | CI gated | CI gated | Stable |
| MP3 and Ogg encoded mutation | FFmpeg 7.1 CI | FFmpeg 7.1 CI | FFmpeg 7.1 CI | Stable with full-decode validation |
| MP4 encoded-video packet mutation | FFmpeg 7.1 CI | FFmpeg 7.1 CI | FFmpeg 7.1 CI | Experimental; packet-only pipelines |
| Matroska decoded video/audio transforms | FFmpeg 7.1 CI | FFmpeg 7.1 CI | FFmpeg 7.1 CI | Stable |
| Matroska encoded-packet mutation | FFmpeg 7.1 CI | FFmpeg 7.1 CI | FFmpeg 7.1 CI | Experimental; unlaced payloads only |
| Expert FFmpeg graphs | Availability checked | Availability checked | Availability checked | Environment-dependent |
| Import-free WebAssembly plugins | Wasmtime CI | Wasmtime CI | Wasmtime CI | ABI v1 |
| AVIF sequence alpha | Requires `avifenc` | Requires `avifenc` | Requires `avifenc` | Tool-dependent |
| Progressive JPEG coefficient mutation | Unavailable | Unavailable | Unavailable | Table-level fallback only |

CI runs `cargo test --all-targets` and strict Clippy on all three operating systems using Rust 1.89.0 and FFmpeg 7.1. Linux additionally gates formatting. These are the release reference environments; other Rust or FFmpeg versions are supported on a best-effort basis and should be checked with `list-formats`, `list-filters`, and representative full transformations.

See the [Threat Model](threat-model.md) for trust boundaries and residual platform risks, and [Migrating to v0.6](migration-v0.6.md) for compatibility changes.