use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{DatabenderError, Result};

const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct JobRecord {
    pub input: PathBuf,
    pub output: PathBuf,
    pub seed: u64,
    pub status: JobStatus,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalState {
    version: u32,
    jobs: Vec<JobRecord>,
}

impl Default for LocalState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            jobs: Vec::new(),
        }
    }
}

impl LocalState {
    pub fn jobs(&self) -> &[JobRecord] {
        &self.jobs
    }

    pub fn record(&mut self, job: JobRecord, limit: usize) -> Result<()> {
        if limit == 0 {
            return Err(invalid_state("job history limit must be at least 1"));
        }
        self.jobs.push(job);
        if self.jobs.len() > limit {
            self.jobs.drain(..self.jobs.len() - limit);
        }
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let state: Self = serde_json::from_slice(&encoded).map_err(|error| {
            invalid_state(format!("could not parse {}: {error}", path.display()))
        })?;
        if state.version != STATE_VERSION {
            return Err(invalid_state(format!(
                "unsupported version {}; expected {STATE_VERSION}",
                state.version
            )));
        }
        Ok(state)
    }

    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| DatabenderError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let mut candidate =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| DatabenderError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        serde_json::to_writer_pretty(candidate.as_file_mut(), self)
            .map_err(|error| invalid_state(format!("could not encode state: {error}")))?;
        candidate
            .as_file_mut()
            .sync_all()
            .map_err(|source| DatabenderError::Io {
                path: candidate.path().to_path_buf(),
                source,
            })?;
        candidate
            .persist(path)
            .map_err(|error| DatabenderError::Io {
                path: path.to_path_buf(),
                source: error.error,
            })?;
        Ok(())
    }
}

fn invalid_state(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid local state: {}", reason.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seed: u64) -> JobRecord {
        JobRecord {
            input: PathBuf::from(format!("input-{seed}.png")),
            output: PathBuf::from(format!("output-{seed}.png")),
            seed,
            status: JobStatus::Succeeded,
            diagnostic: None,
        }
    }

    #[test]
    fn persists_only_the_newest_bounded_job_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/jobs.json");
        let mut state = LocalState::default();
        state.record(record(1), 2).unwrap();
        state.record(record(2), 2).unwrap();
        state.record(record(3), 2).unwrap();

        state.write(&path).unwrap();
        let loaded = LocalState::load(path).unwrap();

        assert_eq!(loaded.jobs(), &[record(2), record(3)]);
    }

    #[test]
    fn rejects_unknown_state_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        fs::write(&path, br#"{"version":2,"jobs":[]}"#).unwrap();

        assert!(LocalState::load(path)
            .unwrap_err()
            .to_string()
            .contains("unsupported version 2"));
    }
}
