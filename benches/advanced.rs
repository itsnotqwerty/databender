use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    hint::black_box,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use databender::{
    codecs::encoded_video::mutate_packet,
    ffmpeg::ToolRunner,
    plugin::{PluginPixelFormat, PluginValue, PLUGIN_ABI_VERSION},
    CancellationToken, FilterSpec, PluginInvocation, PluginMedia, PluginSandboxLimits,
    TransformRequest, WasmPluginRuntime,
};
use image::{codecs::avif::AvifEncoder, ExtendedColorType, ImageEncoder};

const PACKET_ITERATIONS: u32 = 10_000;
const SANDBOX_ITERATIONS: u32 = 100;
const MEDIA_ITERATIONS: u32 = 5;

fn main() {
    println!("databender advanced benchmark");
    benchmark_packet_mutation();
    benchmark_sandbox();
    if let Err(error) = benchmark_native_image_validation() {
        println!("native image validation\tunavailable\t{error}");
    }
    if let Err(error) = benchmark_remux_and_validation() {
        println!("remux/validation\tunavailable\t{error}");
    }
    if let Err(error) = benchmark_native_reconstruction() {
        println!("native reconstruction\tunavailable\t{error}");
    }
}

fn benchmark_native_reconstruction() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    let baseline = workspace.path().join("baseline.jpg");
    let input = workspace.path().join("progressive.jpg");
    let image = image::ImageBuffer::from_fn(512, 512, |x, y| {
        image::Rgb([
            (x.wrapping_mul(7) & 0xff) as u8,
            (y.wrapping_mul(11) & 0xff) as u8,
            (x.wrapping_add(y).wrapping_mul(5) & 0xff) as u8,
        ])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85).encode_image(&image)?;
    fs::write(&baseline, encoded)?;
    let status = Command::new("jpegtran")
        .arg("-progressive")
        .arg("-outfile")
        .arg(&input)
        .arg(&baseline)
        .status()?;
    if !status.success() {
        return Err("jpegtran could not generate the progressive fixture".into());
    }

    let filter = FilterSpec::parse(
        "huffman-glitch:engine=coefficient,swaps=128,intensity=0.5,scan_start=1,scan_count=4,frequency_start=1,frequency_end=32",
    )?;
    let started = Instant::now();
    for iteration in 0..MEDIA_ITERATIONS {
        let output = workspace
            .path()
            .join(format!("reconstructed-{iteration}.jpg"));
        let prepared =
            TransformRequest::new(&input, &output, vec![filter.clone()], 42).prepare()?;
        databender::codecs::execute(prepared)?;
        black_box(fs::metadata(output)?.len());
    }
    print_rate(
        "progressive JPEG coefficient reconstruction + validation",
        MEDIA_ITERATIONS,
        started.elapsed(),
    );
    Ok(())
}

fn benchmark_native_image_validation() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    let input = workspace.path().join("input.avif");
    let mut encoded = Vec::new();
    let pixels = vec![128_u8; 512 * 512 * 4];
    AvifEncoder::new_with_speed_quality(&mut encoded, 10, 75).write_image(
        &pixels,
        512,
        512,
        ExtendedColorType::Rgba8,
    )?;
    fs::write(&input, encoded)?;

    let started = Instant::now();
    for iteration in 0..MEDIA_ITERATIONS {
        let output = workspace.path().join(format!("native-{iteration}.avif"));
        let prepared = TransformRequest::new(
            &input,
            &output,
            vec![FilterSpec::Invert],
            u64::from(iteration),
        )
        .prepare()?;
        databender::codecs::execute(prepared)?;
        black_box(fs::metadata(output)?.len());
    }
    print_rate(
        "native AVIF transform + validation",
        MEDIA_ITERATIONS,
        started.elapsed(),
    );
    Ok(())
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
