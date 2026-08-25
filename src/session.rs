use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

use crate::{DatabenderError, FilterSpec, MediaFormat, PipelinePlan, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct TransformRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub filters: Vec<FilterSpec>,
    pub seed: u64,
    pub force: bool,
}

impl TransformRequest {
    pub fn new(
        input: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        filters: Vec<FilterSpec>,
        seed: u64,
    ) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            filters,
            seed,
            force: true,
        }
    }

    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    pub fn with_output_protection(mut self, protect: bool) -> Self {
        self.force = !protect;
        self
    }

    pub fn prepare(self) -> Result<PreparedTransform> {
        let input = canonicalize(&self.input)?;
        let output = normalize_output(&self.output)?;

        if output.exists() && canonicalize(&output)? == input || output == input {
            return Err(DatabenderError::InputEqualsOutput { path: input });
        }
        if output.exists() && !self.force {
            return Err(DatabenderError::OutputExists { path: output });
        }

        let format = MediaFormat::detect(&input)?;
        let plan = PipelinePlan::build(format, self.filters, self.seed)?;

        Ok(PreparedTransform {
            input,
            output,
            force: self.force,
            plan,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreparedTransform {
    input: PathBuf,
    output: PathBuf,
    force: bool,
    plan: PipelinePlan,
}

impl PreparedTransform {
    pub fn input(&self) -> &Path {
        &self.input
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    pub fn plan(&self) -> &PipelinePlan {
        &self.plan
    }

    pub fn publish_with<W, V>(self, write_candidate: W, validate_candidate: V) -> Result<PathBuf>
    where
        W: FnOnce(&mut File) -> std::io::Result<()>,
        V: FnOnce(&Path) -> Result<()>,
    {
        let parent = self
            .output
            .parent()
            .expect("normalized output always has a parent");
        let mut candidate =
            NamedTempFile::new_in(parent).map_err(|source| DatabenderError::Io {
                path: parent.to_path_buf(),
                source,
            })?;

        write_candidate(candidate.as_file_mut()).map_err(|source| DatabenderError::Io {
            path: candidate.path().to_path_buf(),
            source,
        })?;
        candidate
            .as_file_mut()
            .flush()
            .and_then(|()| candidate.as_file().sync_all())
            .map_err(|source| DatabenderError::Io {
                path: candidate.path().to_path_buf(),
                source,
            })?;
        validate_candidate(candidate.path())?;

        let persisted = if self.force {
            candidate.persist(&self.output)
        } else {
            candidate.persist_noclobber(&self.output)
        };
        persisted.map_err(|error| {
            if !self.force && error.error.kind() == std::io::ErrorKind::AlreadyExists {
                DatabenderError::OutputExists {
                    path: self.output.clone(),
                }
            } else {
                DatabenderError::Io {
                    path: self.output.clone(),
                    source: error.error,
                }
            }
        })?;

        Ok(self.output)
    }

    pub fn publish_path_with<W, V>(
        self,
        write_candidate: W,
        validate_candidate: V,
    ) -> Result<PathBuf>
    where
        W: FnOnce(&Path) -> Result<()>,
        V: FnOnce(&Path) -> Result<()>,
    {
        let parent = self
            .output
            .parent()
            .expect("normalized output always has a parent");
        let mut candidate =
            NamedTempFile::new_in(parent).map_err(|source| DatabenderError::Io {
                path: parent.to_path_buf(),
                source,
            })?;

        write_candidate(candidate.path())?;
        candidate
            .as_file_mut()
            .flush()
            .and_then(|()| candidate.as_file().sync_all())
            .map_err(|source| DatabenderError::Io {
                path: candidate.path().to_path_buf(),
                source,
            })?;
        validate_candidate(candidate.path())?;

        let persisted = if self.force {
            candidate.persist(&self.output)
        } else {
            candidate.persist_noclobber(&self.output)
        };
        persisted.map_err(|error| {
            if !self.force && error.error.kind() == std::io::ErrorKind::AlreadyExists {
                DatabenderError::OutputExists {
                    path: self.output.clone(),
                }
            } else {
                DatabenderError::Io {
                    path: self.output.clone(),
                    source: error.error,
                }
            }
        })?;

        Ok(self.output)
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn normalize_output(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| DatabenderError::InvalidParameter {
            parameter: "output".to_owned(),
            reason: "expected a file path".to_owned(),
        })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(canonicalize(parent)?.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_request(directory: &Path, output_name: &str) -> TransformRequest {
        let input = directory.join("input.bin");
        fs::write(&input, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        TransformRequest::new(
            input,
            directory.join(output_name),
            vec![FilterSpec::ByteNoise { probability: 0.05 }],
            42,
        )
    }

    #[test]
    fn prepares_from_content_and_publishes_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let prepared = png_request(directory.path(), "output.png")
            .prepare()
            .unwrap();
        assert_eq!(prepared.plan().format, MediaFormat::Png);

        let output = prepared
            .publish_with(
                |file| file.write_all(b"validated output"),
                |path| {
                    assert_eq!(fs::read(path).unwrap(), b"validated output");
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(fs::read(output).unwrap(), b"validated output");
    }

    #[test]
    fn default_overwrites_existing_but_still_refuses_same_file() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing.png");
        fs::write(&existing, b"keep me").unwrap();

        let prepared = png_request(directory.path(), "existing.png")
            .prepare()
            .unwrap();
        prepared
            .publish_with(|file| file.write_all(b"replacement"), |_| Ok(()))
            .unwrap();
        assert_eq!(fs::read(&existing).unwrap(), b"replacement");

        let input = directory.path().join("input.bin");
        let error = TransformRequest::new(
            &input,
            &input,
            vec![FilterSpec::ByteNoise { probability: 0.05 }],
            42,
        )
        .prepare()
        .unwrap_err();
        assert!(matches!(error, DatabenderError::InputEqualsOutput { .. }));
    }

    #[test]
    fn failed_validation_does_not_publish_a_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.png");
        let prepared = png_request(directory.path(), "output.png")
            .prepare()
            .unwrap();

        let error = prepared
            .publish_with(
                |file| file.write_all(b"invalid output"),
                |_| {
                    Err(DatabenderError::OutputValidation {
                        reason: "fixture rejection".to_owned(),
                    })
                },
            )
            .unwrap_err();

        assert!(matches!(error, DatabenderError::OutputValidation { .. }));
        assert!(!output.exists());
    }

    #[test]
    fn publishes_a_path_written_candidate_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.png");
        let prepared = png_request(directory.path(), "output.png")
            .prepare()
            .unwrap();

        let published = prepared
            .publish_path_with(
                |candidate| {
                    fs::write(candidate, b"encoded by external tool").map_err(|source| {
                        DatabenderError::Io {
                            path: candidate.to_path_buf(),
                            source,
                        }
                    })
                },
                |candidate| {
                    assert_eq!(fs::read(candidate).unwrap(), b"encoded by external tool");
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(published, output);
        assert_eq!(fs::read(output).unwrap(), b"encoded by external tool");
    }

    #[test]
    fn protected_output_refuses_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.png");
        fs::write(&output, b"old output").unwrap();
        let error = png_request(directory.path(), "output.png")
            .with_force(false)
            .prepare()
            .unwrap_err();

        assert!(matches!(error, DatabenderError::OutputExists { .. }));
        assert_eq!(fs::read(output).unwrap(), b"old output");
    }

    #[test]
    fn does_not_clobber_output_created_after_preflight() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output.png");
        let prepared = png_request(directory.path(), "output.png")
            .with_force(false)
            .prepare()
            .unwrap();
        fs::write(&output, b"racing output").unwrap();

        let error = prepared
            .publish_with(|file| file.write_all(b"candidate"), |_| Ok(()))
            .unwrap_err();

        assert!(matches!(error, DatabenderError::OutputExists { .. }));
        assert_eq!(fs::read(output).unwrap(), b"racing output");
    }
}
