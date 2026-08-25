use crate::{
    codecs::capabilities_for,
    error::{DatabenderError, Result},
    filters::{FilterDomain, FilterSpec},
    media::{MediaFormat, StreamKind},
};

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineStage {
    pub domain: FilterDomain,
    pub target: StreamKind,
    pub seed: u64,
    pub filters: Vec<FilterSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelinePlan {
    pub format: MediaFormat,
    pub seed: u64,
    pub stages: Vec<PipelineStage>,
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
                let stage_seed = derive_seed(seed, stages.len() as u64);
                stages.push(PipelineStage {
                    domain,
                    target,
                    seed: stage_seed,
                    filters: vec![filter],
                });
            }
        }

        Ok(Self {
            format,
            seed,
            stages,
        })
    }
}

fn derive_seed(seed: u64, stage_index: u64) -> u64 {
    let mut value = seed
        .wrapping_add(stage_index)
        .wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
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
        assert_ne!(first.stages[0].seed, first.stages[1].seed);
    }
}
