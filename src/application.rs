use std::path::{Path, PathBuf};

use crate::ffmpeg::{AudioStreamInfo, BasicMetadata, ToolRunner, VideoStreamInfo};
use crate::{
    codecs, load_preset, BatchReport, BatchRequest, CancellationToken, CodecCapabilities,
    DatabenderError, FilterSpec, MediaFormat, OutputPolicy, PipelinePlan, PluginRegistry,
    PluginRegistryConfig, Result, TransformRequest,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaProbe {
    pub format: MediaFormat,
    pub capabilities: CodecCapabilities,
    pub available: bool,
    pub dimensions: Option<(u32, u32)>,
    pub video_stream: Option<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    pub metadata: Option<BasicMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineOptions {
    pub config: Option<PathBuf>,
    pub preset: Option<String>,
    pub filters: Vec<String>,
    pub seed: Option<u64>,
    pub protect_output: bool,
    pub plugin_directories: Vec<PathBuf>,
    pub disabled_plugins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPipeline {
    pub filters: Vec<FilterSpec>,
    pub filter_specifications: Vec<String>,
    pub seed: u64,
    pub protect_output: bool,
    pub plugins: PluginRegistry,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgressEvent {
    JobStarted {
        input: PathBuf,
        output: PathBuf,
        seed: u64,
    },
    JobFinished {
        input: PathBuf,
        output: PathBuf,
        seed: u64,
    },
    BatchFinished {
        total: usize,
        failures: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransformResult {
    pub output: PathBuf,
    pub plan: PipelinePlan,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationCommand {
    Plan {
        format: MediaFormat,
        filters: Vec<FilterSpec>,
        seed: u64,
    },
    Transform(TransformRequest),
    Batch(BatchRequest),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationResult {
    Plan(PipelinePlan),
    Transform(TransformResult),
    Batch(BatchReport),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ApplicationService;

impl ApplicationService {
    pub fn execute(
        &self,
        command: ApplicationCommand,
        cancellation: CancellationToken,
        mut progress: impl FnMut(ProgressEvent),
    ) -> Result<ApplicationResult> {
        match command {
            ApplicationCommand::Plan {
                format,
                filters,
                seed,
            } => self
                .build_plan(format, filters, seed)
                .map(ApplicationResult::Plan),
            ApplicationCommand::Transform(request) => self
                .run_transform_cancellable(request, cancellation, &mut progress)
                .map(ApplicationResult::Transform),
            ApplicationCommand::Batch(request) => self
                .run_batch_cancellable(request, cancellation, &mut progress)
                .map(ApplicationResult::Batch),
        }
    }

    pub fn probe(&self, path: impl AsRef<Path>) -> Result<MediaProbe> {
        let path = path.as_ref();
        let format = MediaFormat::detect(path)?;
        let dimensions = matches!(
            format,
            MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::WebP | MediaFormat::Avif
        )
        .then(|| image::image_dimensions(path).ok())
        .flatten();
        let runner = ToolRunner::default();
        let ffmpeg_media = matches!(
            format,
            MediaFormat::Mp3 | MediaFormat::Mp4 | MediaFormat::Ogg | MediaFormat::Matroska
        );
        let video_stream = matches!(format, MediaFormat::Mp4 | MediaFormat::Matroska)
            .then(|| runner.probe_video(path).ok())
            .flatten();
        let audio_streams = if ffmpeg_media {
            runner.probe_audio_streams(path).unwrap_or_default()
        } else {
            Vec::new()
        };
        let metadata = ffmpeg_media
            .then(|| runner.probe_basic_metadata(path).ok())
            .flatten();
        Ok(MediaProbe {
            format,
            capabilities: codecs::capabilities_for(format),
            available: codecs::is_available(format),
            dimensions,
            video_stream,
            audio_streams,
            metadata,
        })
    }

    pub fn build_plan(
        &self,
        format: MediaFormat,
        filters: Vec<FilterSpec>,
        seed: u64,
    ) -> Result<PipelinePlan> {
        PipelinePlan::build(format, filters, seed)
    }

    pub fn resolve_pipeline(&self, options: PipelineOptions) -> Result<ResolvedPipeline> {
        let preset = match (options.config.as_deref(), options.preset.as_deref()) {
            (Some(path), Some(name)) => Some(load_preset(path, name)?),
            _ => None,
        };
        let mut filters = preset
            .as_ref()
            .map(|preset| preset.filters.clone())
            .unwrap_or_default();
        let mut filter_specifications = preset
            .as_ref()
            .map(|preset| preset.filter_specifications.clone())
            .unwrap_or_default();
        filters.extend(
            options
                .filters
                .iter()
                .map(|filter| FilterSpec::parse(filter))
                .collect::<Result<Vec<_>>>()?,
        );
        filter_specifications.extend(options.filters.iter().cloned());
        if filters.is_empty() {
            return Err(DatabenderError::InvalidParameter {
                parameter: "filter".to_owned(),
                reason: "provide at least one --filter or select a preset".to_owned(),
            });
        }
        let mut plugin_config = preset
            .as_ref()
            .map(|preset| preset.plugins.clone())
            .unwrap_or_else(PluginRegistryConfig::default);
        plugin_config.merge(options.plugin_directories, options.disabled_plugins);
        let plugins = PluginRegistry::discover(&plugin_config)?;
        Ok(ResolvedPipeline {
            filters,
            filter_specifications,
            seed: options
                .seed
                .or_else(|| preset.as_ref().and_then(|preset| preset.seed))
                .unwrap_or_else(rand::random),
            protect_output: options.protect_output
                || preset
                    .as_ref()
                    .is_some_and(|preset| preset.output_policy == OutputPolicy::Protect),
            plugins,
        })
    }

    pub fn run_transform(
        &self,
        request: TransformRequest,
        progress: impl FnMut(ProgressEvent),
    ) -> Result<TransformResult> {
        self.run_transform_cancellable(request, CancellationToken::default(), progress)
    }

    pub fn run_transform_cancellable(
        &self,
        request: TransformRequest,
        cancellation: CancellationToken,
        mut progress: impl FnMut(ProgressEvent),
    ) -> Result<TransformResult> {
        let input = request.input.clone();
        let output = request.output.clone();
        let prepared = request.with_cancellation(cancellation).prepare()?;
        let plan = prepared.plan().clone();
        progress(ProgressEvent::JobStarted {
            input: input.clone(),
            output: output.clone(),
            seed: plan.seed,
        });
        let output = codecs::execute(prepared)?;
        progress(ProgressEvent::JobFinished {
            input,
            output: output.clone(),
            seed: plan.seed,
        });
        Ok(TransformResult { output, plan })
    }

    pub fn run_batch(
        &self,
        request: BatchRequest,
        progress: impl FnMut(ProgressEvent),
    ) -> Result<BatchReport> {
        self.run_batch_cancellable(request, CancellationToken::default(), progress)
    }

    pub fn run_batch_cancellable(
        &self,
        request: BatchRequest,
        cancellation: CancellationToken,
        mut progress: impl FnMut(ProgressEvent),
    ) -> Result<BatchReport> {
        let report = request.execute_with_cancellation(cancellation)?;
        progress(ProgressEvent::BatchFinished {
            total: report.items.len(),
            failures: report.failures(),
        });
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use image::{ImageBuffer, Rgba};

    use super::*;

    #[test]
    fn resolves_and_runs_a_transform_with_typed_progress() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.png");
        let output = directory.path().join("output.png");
        ImageBuffer::from_pixel(2, 2, Rgba([10_u8, 20, 30, 255]))
            .save(&input)
            .unwrap();
        let service = ApplicationService;
        let resolved = service
            .resolve_pipeline(PipelineOptions {
                config: None,
                preset: None,
                filters: vec!["channel-shift:pixels=4".to_owned()],
                seed: Some(42),
                protect_output: false,
                plugin_directories: Vec::new(),
                disabled_plugins: Vec::new(),
            })
            .unwrap();
        assert_eq!(resolved.filter_specifications, ["channel-shift:pixels=4"]);
        let request = TransformRequest::new(&input, &output, resolved.filters, resolved.seed);
        let mut events = Vec::new();

        let result = service
            .run_transform(request, |event| events.push(event))
            .unwrap();

        assert_eq!(result.output, output);
        assert_eq!(result.plan.seed, 42);
        assert!(fs::metadata(&result.output).is_ok());
        assert!(matches!(
            events[0],
            ProgressEvent::JobStarted { seed: 42, .. }
        ));
        assert!(matches!(
            events[1],
            ProgressEvent::JobFinished { seed: 42, .. }
        ));
    }

    #[test]
    fn pre_cancelled_transform_does_not_start_or_publish() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.png");
        let output = directory.path().join("output.png");
        ImageBuffer::from_pixel(2, 2, Rgba([10_u8, 20, 30, 255]))
            .save(&input)
            .unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let request = TransformRequest::new(&input, &output, vec![FilterSpec::Invert], 42);
        let mut events = Vec::new();

        let error = ApplicationService
            .run_transform_cancellable(request, cancellation, |event| events.push(event))
            .unwrap_err();

        assert!(matches!(error, DatabenderError::Cancelled));
        assert!(events.is_empty());
        assert!(!output.exists());
    }

    #[test]
    fn shared_command_dispatch_returns_typed_plan_results() {
        let result = ApplicationService
            .execute(
                ApplicationCommand::Plan {
                    format: MediaFormat::Png,
                    filters: vec![FilterSpec::Invert],
                    seed: 42,
                },
                CancellationToken::default(),
                |_| {},
            )
            .unwrap();

        let ApplicationResult::Plan(plan) = result else {
            panic!("expected a plan result");
        };
        assert_eq!(plan.format, MediaFormat::Png);
        assert_eq!(plan.seed, 42);
    }
}
