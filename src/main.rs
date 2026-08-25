use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};
use databender::{
    codecs, load_plugin_config, ApplicationCommand, ApplicationResult, ApplicationService,
    BatchLayout, BatchReport, BatchRequest, CancellationToken, FilterSpec, MediaFormat,
    PipelineOptions, PluginCompatibility, PluginRegistry, PluginRegistryConfig, TransformRequest,
    FFMPEG_AUDIO_FILTER_NAMES, IMAGE_FILTER_NAMES, JPEG_HUFFMAN_FILTER_NAMES,
    MP3_ENCODED_FILTER_NAMES, OGG_ENCODED_FILTER_NAMES, PAYLOAD_FILTER_NAMES,
    PCM_AUDIO_FILTER_NAMES, VIDEO_ENCODED_FILTER_NAMES, VIDEO_FILTER_NAMES,
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
    /// Discover plugins and display compatibility, state, filters, and provenance.
    ListPlugins(PluginArgs),
    /// Validate and display an ordered pipeline without touching media.
    Plan(PlanArgs),
    /// Apply an ordered filter pipeline to a supported image, audio, or video file.
    Transform(TransformArgs),
    /// Transform explicit files or recursively expanded directories.
    Batch(BatchArgs),
    /// Browse and inspect media in an interactive terminal interface.
    Tui(TuiArgs),
}

#[derive(Clone, Debug, Default, Args)]
struct PluginArgs {
    /// Directory containing paired *.plugin.json manifests and *.wasm modules.
    #[arg(long = "plugin-dir")]
    directories: Vec<PathBuf>,

    /// Plugin ID to disable after discovery. Repeat for multiple plugins.
    #[arg(long = "disable-plugin")]
    disabled: Vec<String>,

    /// Load global plugin discovery settings from a TOML configuration.
    #[arg(long = "plugin-config")]
    plugin_config: Option<PathBuf>,
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

    /// TOML configuration containing the selected preset.
    #[arg(long, requires = "preset")]
    config: Option<PathBuf>,

    /// Named preset to load from --config.
    #[arg(long, requires = "config")]
    preset: Option<String>,

    /// Seed used for deterministic filters. A generated seed is printed when omitted.
    #[arg(long)]
    seed: Option<u64>,

    /// Filter specification. Repeat to create an ordered pipeline.
    #[arg(long = "filter")]
    filters: Vec<String>,

    /// Zero-based video stream to transform. Repeat to select multiple streams.
    #[arg(long = "video-stream")]
    video_streams: Vec<usize>,

    /// Refuse to replace an existing destination.
    #[arg(long)]
    protect_output: bool,

    #[command(flatten)]
    plugins: PluginArgs,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum LayoutArg {
    #[default]
    Flat,
    Mirrored,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ThemeArg {
    #[default]
    Standard,
    HighContrast,
    Monochrome,
}

impl From<ThemeArg> for databender::tui::TuiTheme {
    fn from(theme: ThemeArg) -> Self {
        match theme {
            ThemeArg::Standard => Self::Standard,
            ThemeArg::HighContrast => Self::HighContrast,
            ThemeArg::Monochrome => Self::Monochrome,
        }
    }
}

impl From<LayoutArg> for BatchLayout {
    fn from(layout: LayoutArg) -> Self {
        match layout {
            LayoutArg::Flat => Self::Flat,
            LayoutArg::Mirrored => Self::Mirrored,
        }
    }
}

#[derive(Debug, Args)]
struct BatchArgs {
    /// Input files or directories. Directories are expanded recursively.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory receiving transformed files.
    #[arg(long)]
    output_dir: PathBuf,

    /// Flat file names or paths mirrored relative to --root.
    #[arg(long, value_enum, default_value_t)]
    layout: LayoutArg,

    /// Source root used by the mirrored layout.
    #[arg(long)]
    root: Option<PathBuf>,

    /// Maximum number of files transformed concurrently.
    #[arg(long, default_value_t = 1)]
    jobs: usize,

    /// Validate expansion, output paths, and collisions without transforming files.
    #[arg(long)]
    dry_run: bool,

    /// Emit the complete per-file report as JSON.
    #[arg(long)]
    json: bool,

    /// Atomically write the batch report as a resumable manifest.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Resume successful outputs from a compatible manifest.
    #[arg(long)]
    resume: Option<PathBuf>,

    /// TOML configuration containing the selected preset.
    #[arg(long, requires = "preset")]
    config: Option<PathBuf>,

    /// Named preset to load from --config.
    #[arg(long, requires = "config")]
    preset: Option<String>,

    /// Base seed used to derive a deterministic seed for each input path.
    #[arg(long)]
    seed: Option<u64>,

    /// Filter specification appended after preset filters. Repeat to preserve order.
    #[arg(long = "filter")]
    filters: Vec<String>,

    /// Zero-based video stream to transform. Repeat to select multiple streams.
    #[arg(long = "video-stream")]
    video_streams: Vec<usize>,

    /// Refuse to replace existing destinations.
    #[arg(long)]
    protect_output: bool,

    #[command(flatten)]
    plugins: PluginArgs,
}

#[derive(Debug, Args)]
struct TuiArgs {
    /// Input files or directories. The current directory is used when omitted.
    inputs: Vec<PathBuf>,

    /// Initial directory for transformed outputs.
    #[arg(long, default_value = ".")]
    output_dir: PathBuf,

    /// Disable terminal colors while retaining selection emphasis.
    #[arg(long)]
    no_color: bool,

    /// Color theme for terminal contrast and emphasis.
    #[arg(long, value_enum, default_value_t)]
    theme: ThemeArg,

    /// TOML configuration containing the selected preset.
    #[arg(long, requires = "preset")]
    config: Option<PathBuf>,

    /// Named preset used to initialize the pipeline editor.
    #[arg(long, requires = "config")]
    preset: Option<String>,

    /// Filter specification appended after preset filters.
    #[arg(long = "filter")]
    filters: Vec<String>,

    /// Seed used for immediate pipeline preflight.
    #[arg(long)]
    seed: Option<u64>,

    #[command(flatten)]
    plugins: PluginArgs,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::ListFormats => {
            for format in MediaFormat::ALL {
                let supported = match format {
                    MediaFormat::Jpeg => "pixel + Huffman filters",
                    MediaFormat::Png => "pixel + payload filters",
                    MediaFormat::Wav => "PCM + payload filters",
                    MediaFormat::Mp3 => "encoded main-data + FFmpeg audio filters",
                    MediaFormat::Mp4 => "packet + pixel + PCM + FFmpeg filters",
                    MediaFormat::Matroska => "packet + pixel + PCM + FFmpeg filters",
                    MediaFormat::WebP => "pixel filters",
                    MediaFormat::Avif => "pixel filters",
                    MediaFormat::Ogg => "encoded packet + PCM + FFmpeg audio filters",
                };
                if codecs::is_available(format) {
                    println!("{format}\t{supported}");
                } else {
                    println!("{format}\tunavailable (required FFmpeg codec missing)");
                }
            }
            Ok(())
        }
        Command::ListFilters => {
            list_filters();
            Ok(())
        }
        Command::ListPlugins(args) => list_plugins(args),
        Command::Plan(args) => plan(args),
        Command::Transform(args) => transform(args),
        Command::Batch(args) => batch(args),
        Command::Tui(args) => tui(args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_plugin_config(args: PluginArgs) -> databender::Result<PluginRegistryConfig> {
    let mut config = args
        .plugin_config
        .map(load_plugin_config)
        .transpose()?
        .unwrap_or_default();
    config.merge(args.directories, args.disabled);
    Ok(config)
}

fn list_plugins(args: PluginArgs) -> databender::Result<()> {
    let registry = PluginRegistry::discover(&resolve_plugin_config(args)?)?;
    if registry.plugins().is_empty() {
        println!("no plugins discovered");
        return Ok(());
    }
    for plugin in registry.plugins() {
        let compatibility = match &plugin.compatibility {
            PluginCompatibility::Compatible => "compatible".to_owned(),
            PluginCompatibility::Incompatible(reason) => format!("incompatible: {reason}"),
        };
        let state = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        println!("{}\t{state}\t{compatibility}", plugin.provenance());
        for filter in &plugin.manifest.filters {
            let domains = filter
                .domains
                .iter()
                .map(|domain| format!("{domain:?}"))
                .collect::<Vec<_>>()
                .join(",");
            println!("  {}\t{}\t{domains}", filter.id, filter.name);
        }
    }
    Ok(())
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
    if codecs::is_available(MediaFormat::Mp3) {
        print_filter_group("mp3", "encoded main data", &MP3_ENCODED_FILTER_NAMES);
        print_filter_group("mp3", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    } else {
        println!("mp3 [unavailable: required FFmpeg codec missing]");
    }
    if codecs::is_available(MediaFormat::Ogg) {
        print_filter_group("ogg", "encoded packets", &OGG_ENCODED_FILTER_NAMES);
        print_filter_group("ogg", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
        print_filter_group("ogg", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
    } else {
        println!("ogg [unavailable: required FFmpeg codec missing]");
    }
    if codecs::is_available(MediaFormat::Mp4) {
        print_filter_group("mp4", "encoded video packets", &VIDEO_ENCODED_FILTER_NAMES);
        print_filter_group("mp4", "image pixels", &IMAGE_FILTER_NAMES);
        print_filter_group("mp4", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
        print_filter_group("mp4", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
        print_filter_group("mp4", "FFmpeg video", &VIDEO_FILTER_NAMES);
    } else {
        println!("mp4 [unavailable: required FFmpeg codec missing]");
    }
    if codecs::is_available(MediaFormat::Matroska) {
        print_filter_group("mkv", "encoded video packets", &VIDEO_ENCODED_FILTER_NAMES);
        print_filter_group("mkv", "image pixels", &IMAGE_FILTER_NAMES);
        print_filter_group("mkv", "PCM audio", &PCM_AUDIO_FILTER_NAMES);
        print_filter_group("mkv", "FFmpeg audio", &FFMPEG_AUDIO_FILTER_NAMES);
        print_filter_group("mkv", "FFmpeg video", &VIDEO_FILTER_NAMES);
    } else {
        println!("mkv [unavailable: required FFmpeg codec missing]");
    }
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
    let ApplicationResult::Plan(plan) = ApplicationService.execute(
        ApplicationCommand::Plan {
            format: args.format,
            filters,
            seed: args.seed,
        },
        CancellationToken::default(),
        |_| {},
    )?
    else {
        unreachable!("plan commands return plan results")
    };

    println!("plan-version: {}", plan.version);
    println!("format: {}", plan.format);
    println!("seed: {}", plan.seed);
    println!("environment-dependent: {}", plan.environment_dependent);
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
        if let Some(graph) = &stage.resolved_graph {
            println!("  resolved graph: {graph}");
        }
        for filter in &stage.filters {
            if let Some(estimate) = filter.impact_estimate() {
                println!("  impact estimate: {estimate}");
            }
        }
    }

    Ok(())
}

fn transform(args: TransformArgs) -> databender::Result<()> {
    let service = ApplicationService;
    let plugin_config = resolve_plugin_config(args.plugins)?;
    let resolved = service.resolve_pipeline(PipelineOptions {
        config: args.config,
        preset: args.preset,
        filters: args.filters,
        seed: args.seed,
        protect_output: args.protect_output,
        plugin_directories: plugin_config.directories,
        disabled_plugins: plugin_config.disabled.into_iter().collect(),
    })?;
    let request = TransformRequest::new(args.input, args.output, resolved.filters, resolved.seed)
        .with_output_protection(resolved.protect_output)
        .with_video_streams(args.video_streams);
    let ApplicationResult::Transform(result) = service.execute(
        ApplicationCommand::Transform(request),
        CancellationToken::default(),
        |_| {},
    )?
    else {
        unreachable!("transform commands return transform results")
    };

    println!("output: {}", result.output.display());
    println!("seed: {}", result.plan.seed);
    Ok(())
}

fn batch(args: BatchArgs) -> databender::Result<()> {
    let service = ApplicationService;
    let plugin_config = resolve_plugin_config(args.plugins)?;
    let resolved = service.resolve_pipeline(PipelineOptions {
        config: args.config,
        preset: args.preset,
        filters: args.filters,
        seed: args.seed,
        protect_output: args.protect_output,
        plugin_directories: plugin_config.directories,
        disabled_plugins: plugin_config.disabled.into_iter().collect(),
    })?;
    let resume = args.resume.as_ref().map(BatchReport::load).transpose()?;
    let ApplicationResult::Batch(report) = service.execute(
        ApplicationCommand::Batch(BatchRequest {
            inputs: args.inputs,
            output_directory: args.output_dir,
            root: args.root,
            layout: args.layout.into(),
            filters: resolved.filters,
            video_streams: args.video_streams,
            seed: resolved.seed,
            protect_output: resolved.protect_output,
            jobs: args.jobs,
            dry_run: args.dry_run,
            resume,
        }),
        CancellationToken::default(),
        |_| {},
    )?
    else {
        unreachable!("batch commands return batch results")
    };
    if let Some(path) = args.manifest.as_ref().or(args.resume.as_ref()) {
        report.write(path)?;
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("batch report is serializable")
        );
    } else {
        for item in &report.items {
            if let Some(error) = &item.error {
                eprintln!("failed\t{}\t{error}", item.input.display());
            } else {
                let status = if item.resumed {
                    "resumed"
                } else if item.executed {
                    "ok"
                } else {
                    "planned"
                };
                println!(
                    "{status}\t{}\t{}\tseed={}",
                    item.input.display(),
                    item.output.display(),
                    item.seed
                );
            }
        }
    }
    let failures = report.failures();
    if failures != 0 {
        return Err(databender::DatabenderError::OutputValidation {
            reason: format!("batch completed with {failures} failed item(s)"),
        });
    }
    Ok(())
}

fn tui(args: TuiArgs) -> databender::Result<()> {
    let plugin_config = resolve_plugin_config(args.plugins)?;
    let (filters, filter_specifications, seed, plugins) =
        if args.config.is_some() || args.preset.is_some() || !args.filters.is_empty() {
            let resolved = ApplicationService.resolve_pipeline(PipelineOptions {
                config: args.config,
                preset: args.preset,
                filters: args.filters,
                seed: args.seed,
                protect_output: false,
                plugin_directories: plugin_config.directories.clone(),
                disabled_plugins: plugin_config.disabled.iter().cloned().collect(),
            })?;
            (
                resolved.filters,
                resolved.filter_specifications,
                resolved.seed,
                resolved.plugins,
            )
        } else {
            (
                Vec::new(),
                Vec::new(),
                args.seed.unwrap_or_else(rand::random),
                PluginRegistry::discover(&plugin_config)?,
            )
        };
    let terminal = databender::tui::TerminalCapabilities::detect();
    let theme = if args.no_color || !terminal.color {
        databender::tui::TuiTheme::Monochrome
    } else {
        args.theme.into()
    };
    databender::tui::run_with_plugin_specifications(
        &args.inputs,
        args.output_dir,
        theme,
        filters,
        filter_specifications,
        seed,
        plugins,
    )
}
