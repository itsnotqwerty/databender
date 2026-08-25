use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    hint::black_box,
    path::Path,
    time::{Duration, Instant},
};

use databender::{
    codecs::encoded_video::mutate_packet,
    ffmpeg::ToolRunner,
    plugin::{PluginPixelFormat, PluginValue, PLUGIN_ABI_VERSION},
    CancellationToken, PluginInvocation, PluginMedia, PluginSandboxLimits, WasmPluginRuntime,
};

const PACKET_ITERATIONS: u32 = 10_000;
const SANDBOX_ITERATIONS: u32 = 100;
const MEDIA_ITERATIONS: u32 = 5;

fn main() {
    println!("databender advanced benchmark");
    benchmark_packet_mutation();
    benchmark_sandbox();
    if let Err(error) = benchmark_remux_and_validation() {
        println!("remux/validation\tunavailable\t{error}");
    }
    println!("native reconstruction\tunavailable\tprogressive coefficient engine not implemented");
}

fn benchmark_packet_mutation() {
    let mut nal = vec![0x65; 64 * 1024];
    nal[0] = 0x65;
    let mut packet = Vec::with_capacity(nal.len() + 4);
    packet.extend_from_slice(&(nal.len() as u32).to_be_bytes());
    packet.extend_from_slice(&nal);
    let started = Instant::now();
    let mut changed = 0;
    for seed in 0..PACKET_ITERATIONS {
        let mut candidate = packet.clone();
        changed += mutate_packet("h264", &mut candidate, Some(4), 8, 0.125, u64::from(seed))
            .expect("valid benchmark packet")
            .mutated_bytes;
        black_box(candidate);
    }
    print_rate("packet mutation", PACKET_ITERATIONS, started.elapsed());
    black_box(changed);
}

fn benchmark_sandbox() {
    let wasm =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/image-invert.wasm"))
            .expect("packaged image plugin fixture");
    let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default()).expect("sandbox runtime");
    let invocation = PluginInvocation {
        abi_version: PLUGIN_ABI_VERSION,
        invocation_id: 1,
        filter_id: "invert".to_owned(),
        seed: 42,
        parameters: BTreeMap::<String, PluginValue>::new(),
        input: PluginMedia::ImageFrame {
            width: 1,
            height: 1,
            stride: 4,
            pixel_format: PluginPixelFormat::Rgba8,
            data: vec![10, 20, 30, 255],
        },
    };
    let started = Instant::now();
    for _ in 0..SANDBOX_ITERATIONS {
        black_box(
            runtime
                .execute(&wasm, &invocation, &CancellationToken::default())
                .expect("valid sandbox invocation"),
        );
    }
    print_rate("sandbox invocation", SANDBOX_ITERATIONS, started.elapsed());
}

fn benchmark_remux_and_validation() -> Result<(), Box<dyn std::error::Error>> {
    let runner = ToolRunner::default();
    if !runner.supports_encoder("libx264") {
        return Err("libx264 encoder is unavailable".into());
    }
    let workspace = tempfile::tempdir()?;
    let input = workspace.path().join("input.mp4");
    runner.ffmpeg([
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-f"),
        OsString::from("lavfi"),
        OsString::from("-i"),
        OsString::from("testsrc=size=320x180:rate=24:duration=2"),
        OsString::from("-codec:v"),
        OsString::from("libx264"),
        OsString::from("-pix_fmt"),
        OsString::from("yuv420p"),
        input.as_os_str().to_owned(),
    ])?;

    let remux_started = Instant::now();
    for iteration in 0..MEDIA_ITERATIONS {
        let output = workspace.path().join(format!("remux-{iteration}.mp4"));
        runner.ffmpeg([
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-y"),
            OsString::from("-i"),
            input.as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from("0"),
            OsString::from("-codec"),
            OsString::from("copy"),
            output.as_os_str().to_owned(),
        ])?;
        black_box(fs::metadata(output)?.len());
    }
    print_rate(
        "stream-copy remux",
        MEDIA_ITERATIONS,
        remux_started.elapsed(),
    );

    let validation_started = Instant::now();
    for _ in 0..MEDIA_ITERATIONS {
        black_box(runner.probe_video_streams(&input)?);
        runner.ffmpeg([
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-i"),
            input.as_os_str().to_owned(),
            OsString::from("-map"),
            OsString::from("0:v"),
            OsString::from("-f"),
            OsString::from("null"),
            OsString::from("-"),
        ])?;
    }
    print_rate(
        "probe + full decode",
        MEDIA_ITERATIONS,
        validation_started.elapsed(),
    );
    Ok(())
}

fn print_rate(name: &str, iterations: u32, elapsed: Duration) {
    let per_iteration = elapsed.as_secs_f64() * 1_000.0 / f64::from(iterations);
    println!(
        "{name}\t{iterations} iterations\t{:.3} ms/op\t{:.3} s total",
        per_iteration,
        elapsed.as_secs_f64()
    );
}
