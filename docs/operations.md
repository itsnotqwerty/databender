# Operational Limits

These measurements are a reproducible baseline, not fixed limits. They were collected from an optimized build on an Intel Core i7-9700K with Rust 1.97.1 and FFmpeg 9.0.1. Peak RSS is the maximum resident set reported for the Databender child process and the FFmpeg processes it waited for.

## Baseline

| Workload | Pipeline | Elapsed | Throughput | Peak RSS | Input | Output |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 2048x2048 AVIF still | `invert` | 7.77 s | 0.54 megapixels/s | 155 MiB | 41.9 KiB | 139.2 KiB |
| 120 s stereo Opus/Ogg at 48 kHz | `audio-noise`, `high-pass` | 2.50 s | 48x real time | 48.9 MiB | 1.70 MiB | 1.66 MiB |
| 8 s 640x360 Matroska at 24 fps, FFV1 plus three FLAC streams | `invert`, `audio-noise`, `high-pass` | 2.86 s | 2.8x real time | 111 MiB | 3.53 MiB | 6.09 MiB |

## Advanced Features

Run the stable custom benchmark with:

```bash
cargo bench --bench advanced
```

It measures a 64 KiB H.264 packet with an eight-byte mutation budget, complete compile/instantiate/execute/validate cycles for the packaged image plugin, five native 512x512 AVIF transforms, five progressive 512x512 JPEG coefficient reconstructions, five stream-copy remuxes of a generated two-second 320x180 H.264 MP4, and five ffprobe plus complete FFmpeg decode validations. On the baseline host described above:

| Operation | Iterations | Mean |
| --- | ---: | ---: |
| H.264 packet mutation | 10,000 | 0.055 ms |
| WebAssembly sandbox invocation | 100 | 1.265 ms |
| Native AVIF transform plus validation | 5 | 535.091 ms |
| Progressive JPEG coefficient reconstruction plus validation | 5 | 29.880 ms |
| MP4 stream-copy remux | 5 | 57.380 ms |
| Video probe plus complete decode | 5 | 138.403 ms |

The executable reports unavailable components instead of silently omitting them. The reconstruction fixture uses `jpegtran` to produce progressive input; the codec itself remains pure Rust. Numbers include Wasmtime compilation for every sandbox invocation and process startup for every remux, probe, and decode, matching current execution paths rather than an amortized lower bound.

Advanced feature stability tiers are:

- **Stable:** native structural parsers, typed image/audio filters, MP3/Ogg encoded mutation, atomic publication, and plugin ABI v1 validation.
- **Experimental:** progressive JPEG coefficient mutation plus MP4 and Matroska encoded-video packet mutation. JPEG output is sequential and independently decoded; packet mutation remains codec-dependent.
- **Environment-dependent:** FFmpeg remuxing, expert graphs, and encoded bytes produced by external codecs. Geometry and metadata contracts are stable; compressed bytes are not cross-build golden values.

Benchmark results are diagnostic baselines, not pass/fail thresholds. Record the CPU, filesystem, Rust version, FFmpeg version, and enabled encoders when comparing releases.

Fixtures were generated with FFmpeg test sources and transformed with seed 42. The AVIF source used one 2048x2048 `testsrc2` frame and `libaom-av1`. Ogg used a 120-second 48 kHz sine encoded by `libopus`. Matroska used 8 seconds of 640x360 `testsrc2` at 24 fps, FFV1 video, and three 48 kHz FLAC sine streams. Run release builds for representative numbers:

```bash
cargo build --release
./target/release/databender transform INPUT --output OUTPUT \
	--seed 42 --filter FILTER
```

## Scaling

Still-image pixel stages hold decoded RGBA pixels and encoded buffers in memory. The base decoded allocation is approximately:

$$
M_{image} = width \times height \times 4\ \text{bytes}
$$

The AVIF encoder uses additional frame and compression buffers, so observed RSS is substantially above this base. A 2048x2048 RGBA frame is 16 MiB before encoder overhead.

Still JPEG, PNG, WebP, and AVIF decoding rejects either dimension above 16,384 pixels and limits decoder-tracked allocations to 512 MiB. The dimension ceiling is strict for every native decoder; allocation accounting is also enforced where the underlying decoder supports it. This prevents compact files with oversized dimension headers from allocating unbounded pixel buffers before transformation.

PCM intermediates are signed 16-bit WAV files. Excluding small headers, temporary storage per audio stage and stream is approximately:

$$
D_{audio} = duration \times sample\_rate \times channels \times 2\ \text{bytes}
$$

Intermediates remain in the operation workspace until publication, so multiply by the number of decoded audio stages and streams. The 120-second stereo fixture requires about 22 MiB per PCM stage; its two stages therefore require about 44 MiB plus encoded input/output and filesystem overhead.

Native video-frame stages currently materialize both decoded and filtered RGBA streams. Each raw stream requires approximately:

$$
D_{video} = width \times height \times 4 \times frame\_count\ \text{bytes}
$$

For the 640x360, 192-frame fixture, each raw stream is about 169 MiB. A native stage therefore needs roughly 338 MiB for its decoded and filtered raw files, plus FFV1 video, PCM audio stages, source, and candidate output. Longer or higher-resolution video should use a temporary filesystem with ample free space.

Within that disk-backed stage, native filters process ordered batches of at most eight RGBA frames and no more than the available CPU parallelism. In-memory frame storage for this layer is therefore bounded at approximately $workers \times width \times height \times 4$ bytes plus filter overhead. Frame-derived seeds and ordered joins keep output deterministic across worker scheduling.

Batch `--jobs` bounds concurrent files, not aggregate bytes. Worst-case memory and temporary-disk demand scale approximately with the sum of the concurrently active items. Start with `--jobs 1` for AVIF or native video pipelines, then increase only after measuring representative media. FFmpeg-backed compressed output can vary across builds, and storage throughput can dominate the Matroska path.

Animated-image and FFmpeg-backed pipelines do not currently enforce duration, aggregate memory, or free-disk ceilings. Candidate validation, the 120-second subprocess timeout, 64 KiB subprocess-output capture, WebAssembly memory/fuel/message limits, and atomic publication bound individual failure modes, but the operating system can still terminate a process or reject writes when aggregate resources are exhausted.
