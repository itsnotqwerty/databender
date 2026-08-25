use crate::{
    codecs::{capabilities_for, runtime_unavailable_reason},
    error::{DatabenderError, Result},
    filters::{audio::compile_audio_graph, video::compile_video_graph, FilterDomain, FilterSpec},
    media::{MediaFormat, StreamKind},
    seed::{derive_seed, SeedIdentity},
};

pub const PIPELINE_PLAN_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineStage {
    pub domain: FilterDomain,
    pub target: StreamKind,
    pub seed: u64,
    pub filters: Vec<FilterSpec>,
    pub resolved_graph: Option<String>,
    pub environment_dependent: bool,
    pub(crate) index: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelinePlan {
    pub version: u32,
    pub format: MediaFormat,
    pub seed: u64,
    pub stages: Vec<PipelineStage>,
    pub environment_dependent: bool,
}

impl PipelinePlan {
    pub fn build(format: MediaFormat, filters: Vec<FilterSpec>, seed: u64) -> Result<Self> {
        let capabilities = capabilities_for(format);

        for filter in &filters {
            if !capabilities.supports(filter.domain()) {
                return Err(DatabenderError::IncompatibleFilter {
                    filter: filter.name().to_owned(),
                    format: format.to_string(),
                    reason: format!("the {:?} domain is not supported", filter.domain()),
                });
            }
            if let Some(reason) = runtime_unavailable_reason(format, filter) {
                return Err(DatabenderError::IncompatibleFilter {
                    filter: filter.name().to_owned(),
                    format: format.to_string(),
                    reason,
                });
            }
        }

        let mut stages: Vec<PipelineStage> = Vec::new();
        for filter in filters {
            let domain = filter.domain();
            let target = if format == MediaFormat::Wav && domain == FilterDomain::EncodedPayload {
                StreamKind::Audio
            } else {
                filter.target()
            };
            let joins_previous = stages
                .last()
                .is_some_and(|stage| stage.domain == domain && stage.target == target);

            if joins_previous {
                stages
                    .last_mut()
                    .expect("stage exists")
                    .filters
                    .push(filter);
            } else {
                let stage_seed = derive_seed(seed, SeedIdentity::Stage(stages.len() as u64));
                stages.push(PipelineStage {
                    domain,
                    target,
                    seed: stage_seed,
                    filters: vec![filter],
                    resolved_graph: None,
                    environment_dependent: false,
                    index: stages.len() as u64,
                });
            }
        }

        for stage in &mut stages {
            stage.environment_dependent =
                stage.filters.iter().any(FilterSpec::environment_dependent);
            stage.resolved_graph = match stage.domain {
                FilterDomain::FfmpegAudio => Some(compile_audio_graph(&stage.filters)?),
                FilterDomain::FfmpegVideo => Some(compile_video_graph(&stage.filters)?),
                _ => None,
            };
        }
        let environment_dependent = stages.iter().any(|stage| stage.environment_dependent);

        Ok(Self {
            version: PIPELINE_PLAN_VERSION,
            format,
            seed,
            stages,
            environment_dependent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_only_adjacent_filters_in_the_same_domain() {
        let filters = vec![
            FilterSpec::ChannelShift { pixels: 4 },
            FilterSpec::PixelSort { threshold: 128 },
            FilterSpec::ByteNoise { probability: 0.05 },
            FilterSpec::ScanlineDisplacement { max_shift: 12 },
        ];

        let plan = PipelinePlan::build(MediaFormat::Png, filters, 42).unwrap();

        assert_eq!(plan.stages.len(), 3);
        assert_eq!(plan.stages[0].filters.len(), 2);
        assert_eq!(plan.stages[1].domain, FilterDomain::EncodedPayload);
        assert_eq!(plan.stages[2].domain, FilterDomain::ImagePixels);
    }

    #[test]
    fn rejects_encoded_mutation_for_compressed_media() {
        let error = PipelinePlan::build(
            MediaFormat::Mp4,
            vec![FilterSpec::ByteSwap { count: 8 }],
            42,
        )
        .unwrap_err();

        assert!(matches!(error, DatabenderError::IncompatibleFilter { .. }));
    }

    #[test]
    fn distinguishes_jpeg_and_png_payload_capabilities() {
        let filter = vec![FilterSpec::ByteNoise { probability: 0.1 }];

        assert!(PipelinePlan::build(MediaFormat::Jpeg, filter.clone(), 42).is_err());
        assert!(PipelinePlan::build(MediaFormat::Png, filter, 42).is_ok());
    }

    #[test]
    fn supports_huffman_mutation_for_jpeg_only() {
        let filter = vec![FilterSpec::parse("huffman-glitch").unwrap()];

        assert!(PipelinePlan::build(MediaFormat::Jpeg, filter.clone(), 42).is_ok());
        assert!(PipelinePlan::build(MediaFormat::Png, filter, 42).is_err());
    }

    #[test]
    fn derives_repeatable_stage_seeds() {
        let filters = vec![
            FilterSpec::ChannelShift { pixels: 4 },
            FilterSpec::ByteNoise { probability: 0.05 },
        ];

        let first = PipelinePlan::build(MediaFormat::Png, filters.clone(), 123).unwrap();
        let second = PipelinePlan::build(MediaFormat::Png, filters, 123).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.version, PIPELINE_PLAN_VERSION);
        assert_ne!(first.stages[0].seed, first.stages[1].seed);
    }

    #[test]
    fn records_resolved_expert_graphs_and_environment_dependence() {
        let filters = vec![
            FilterSpec::parse("high-pass:frequency=300").unwrap(),
            FilterSpec::parse("expert-audio-graph:volume=1.5,aecho=0.8:0.9:20:0.2").unwrap(),
        ];

        let plan = PipelinePlan::build(MediaFormat::Mp3, filters, 42).unwrap();

        assert!(plan.environment_dependent);
        assert!(plan.stages[0].environment_dependent);
        assert_eq!(
            plan.stages[0].resolved_graph.as_deref(),
            Some("highpass=f=300,volume=1.5,aecho=0.8:0.9:20:0.2")
        );
    }
}
