use crate::{DatabenderError, FilterSpec, Result, VideoEffect};

pub fn compile_video_graph(filters: &[FilterSpec]) -> Result<String> {
    filters
        .iter()
        .map(|filter| match filter {
            FilterSpec::VideoEffect(VideoEffect::Hue { degrees }) => Ok(format!("hue=h={degrees}")),
            FilterSpec::VideoEffect(VideoEffect::Equalize { contrast }) => {
                Ok(format!("eq=contrast={contrast}"))
            }
            FilterSpec::VideoEffect(VideoEffect::Lag { frames }) => {
                Ok(format!("tmix=frames={}", frames + 1))
            }
            FilterSpec::ExpertVideoGraph(graph) => Ok(graph.fragment().to_owned()),
            _ => Err(DatabenderError::OutputValidation {
                reason: format!(
                    "filter {} cannot be compiled into an FFmpeg video graph",
                    filter.name()
                ),
            }),
        })
        .collect::<Result<Vec<_>>>()
        .map(|filters| filters.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_typed_effects_in_order() {
        let filters = [
            FilterSpec::VideoEffect(VideoEffect::Hue { degrees: 45.0 }),
            FilterSpec::VideoEffect(VideoEffect::Equalize { contrast: 1.5 }),
            FilterSpec::VideoEffect(VideoEffect::Lag { frames: 3 }),
        ];

        assert_eq!(
            compile_video_graph(&filters).unwrap(),
            "hue=h=45,eq=contrast=1.5,tmix=frames=4"
        );
    }

    #[test]
    fn rejects_filters_outside_the_ffmpeg_video_domain() {
        let error = compile_video_graph(&[FilterSpec::Invert]).unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot be compiled into an FFmpeg video graph"));
    }
}
