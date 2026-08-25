use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use databender::{
    codecs, FilterSpec, MediaFormat, PipelinePlan, TransformRequest, FFMPEG_AUDIO_FILTER_NAMES,
    IMAGE_FILTER_NAMES, JPEG_HUFFMAN_FILTER_NAMES, PAYLOAD_FILTER_NAMES, PCM_AUDIO_FILTER_NAMES,
    VIDEO_FILTER_NAMES,
};

#[derive(Debug, Parser)]
#[command(name = "databender", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List formats planned for the phased implementation.
    ListFormats,
    /// List filters grouped by compatible media format and processing domain.
    ListFilters,
    /// Validate and display an ordered pipeline without touching media.
    Plan(PlanArgs),
    /// Apply an ordered filter pipeline to a supported image, audio, or video file.
    Transform(TransformArgs),
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// Target media format.
    #[arg(long)]
    format: MediaFormat,

    /// Seed used to derive deterministic stage seeds.
    #[arg(long)]
    seed: u64,

    /// Filter name. Repeat to create an ordered pipeline.
    #[arg(long = "filter", required = true)]
    filters: Vec<String>,
}

#[derive(Debug, Args)]
struct TransformArgs {
    /// Input media file. Its format is detected from content rather than its extension.
    input: PathBuf,

    /// Destination file. Existing files are replaced by default.
    #[arg(long)]
    output: PathBuf,

    /// Seed used for deterministic filters. A generated seed is printed when omitted.
    #[arg(long)]
    seed: Option<u64>,

    /// Filter specification. Repeat to create an ordered pipeline.
    #[arg(long = "filter", required = true)]
    filters: Vec<String>,

    /// Refuse to replace an existing destination.
    #[arg(long)]
    protect_output: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::ListFormats => {
            for format in MediaFormat::ALL {
                let status = match format {
                    MediaFormat::Jpeg => "pixel + Huffman filters",
                    MediaFormat::Png => "pixel + payload filters",
                    MediaFormat::Wav => "PCM + payload filters",
                    MediaFormat::Mp3 => "FFmpeg audio filters",
                    MediaFormat::Mp4 => "pixel + PCM + FFmpeg filters",
                    MediaFormat::Matroska => "pixel + PCM + FFmpeg filters",
                    MediaFormat::WebP => "pixel filters",
                    MediaFormat::Avif => "pixel filters",
                    MediaFormat::Ogg => "PCM + FFmpeg audio filters",
                };
                println!("{format}\t{status}");
            }
            Ok(())
        }
        Command::ListFilters => {
            list_filters();
            Ok(())
        }
        Command::Plan(args) => plan(args),
        Command::Transform(args) => transform(args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn list_filters() {
    print_filter_group("jpeg", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("jpeg", "Huffman tables", &JPEG_HUFFMAN_FILTER_NAMES);
    print_filter_group("png", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("png", "encoded payload", &PAYLOAD_FILTER_NAMES);
    print_filter_group("webp", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("avif", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("wav", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
    print_filter_group("wav", "encoded payload", &PAYLOAD_FILTER_NAMES);
    print_filter_group("mp3", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    print_filter_group("ogg", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
    print_filter_group("ogg", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    print_filter_group("mp4", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("mp4", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
    print_filter_group("mp4", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    print_filter_group("mp4", "FFmpeg video", &VIDEO_FILTER_NAMES);
    print_filter_group("mkv", "image pixels", &IMAGE_FILTER_NAMES);
    print_filter_group("mkv", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
    print_filter_group("mkv", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    print_filter_group("mkv", "FFmpeg video", &VIDEO_FILTER_NAMES);
}

fn print_filter_group(codec: &str, domain: &str, filters: &[&str]) {
    println!("{codec} [{domain}]");
    for filter in filters {
        println!("  {filter}");
    }
}

fn parse_filters(specifications: &[String]) -> databender::Result<Vec<FilterSpec>> {
    specifications
        .iter()
        .map(|specification| FilterSpec::parse(specification))
        .collect()
}

fn plan(args: PlanArgs) -> databender::Result<()> {
    let filters = parse_filters(&args.filters)?;
    let plan = PipelinePlan::build(args.format, filters, args.seed)?;

    println!("format: {}", plan.format);
    println!("seed: {}", plan.seed);
    for (index, stage) in plan.stages.iter().enumerate() {
        let filters = stage
            .filters
            .iter()
            .map(FilterSpec::name)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "stage {}: {:?}/{:?}, seed={}, filters={}",
            index + 1,
            stage.target,
            stage.domain,
            stage.seed,
            filters
        );
    }

    Ok(())
}

fn transform(args: TransformArgs) -> databender::Result<()> {
    let seed = args.seed.unwrap_or_else(rand::random);
    let filters = parse_filters(&args.filters)?;
    let request = TransformRequest::new(args.input, args.output, filters, seed)
        .with_output_protection(args.protect_output);
    let output = codecs::execute(request.prepare()?)?;

    println!("output: {}", output.display());
    println!("seed: {seed}");
    Ok(())
}
