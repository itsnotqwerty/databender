use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
};

use serde::{Deserialize, Serialize};

use crate::{
    codecs,
    filters::{audio::compile_audio_graph, video::compile_video_graph, FilterDomain},
    seed::{derive_seed, SeedIdentity},
    CancellationToken, DatabenderError, FilterSpec, MediaFormat, Result, TransformRequest,
    PIPELINE_PLAN_VERSION,
};

const MANIFEST_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BatchLayout {
    #[default]
    Flat,
    Mirrored,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatchRequest {
    pub inputs: Vec<PathBuf>,
    pub output_directory: PathBuf,
    pub root: Option<PathBuf>,
    pub layout: BatchLayout,
    pub filters: Vec<FilterSpec>,
    pub video_streams: Vec<usize>,
    pub seed: u64,
    pub protect_output: bool,
    pub jobs: usize,
    pub dry_run: bool,
    pub resume: Option<BatchReport>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BatchItemResult {
    pub input: PathBuf,
    pub output: PathBuf,
    pub seed: u64,
    pub executed: bool,
    #[serde(default)]
    pub resumed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BatchReport {
    pub version: u32,
    pub plan_version: u32,
    pub fingerprint: u64,
    #[serde(default)]
    pub environment_dependent: bool,
    #[serde(default)]
    pub resolved_graphs: Vec<BatchResolvedGraph>,
    #[serde(default)]
    pub mutation_impacts: Vec<BatchMutationImpact>,
    pub items: Vec<BatchItemResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BatchResolvedGraph {
    pub target: String,
    pub graph: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BatchMutationImpact {
    pub filter: String,
    pub estimate: String,
}

impl BatchReport {
    pub fn failures(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.error.is_some())
            .count()
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&encoded).map_err(|error| invalid_manifest(path, error))
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
            .map_err(|error| invalid_manifest(path, error))?;
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

impl BatchRequest {
    pub fn execute(self) -> Result<BatchReport> {
        self.execute_with_cancellation(CancellationToken::default())
    }

    pub fn execute_with_cancellation(self, cancellation: CancellationToken) -> Result<BatchReport> {
        cancellation.check()?;
        if self.inputs.is_empty() {
            return Err(invalid("batch requires at least one input"));
        }
        if self.filters.is_empty() {
            return Err(invalid("batch requires at least one filter"));
        }
        if self.jobs == 0 {
            return Err(invalid("jobs must be at least 1"));
        }
        let inputs = expand_inputs(&self.inputs)?;
        if inputs.is_empty() {
            return Err(invalid("input expansion found no files"));
        }
        let output_directory = absolute_directory(&self.output_directory)?;
        let root = match self.layout {
            BatchLayout::Flat => None,
            BatchLayout::Mirrored => Some(canonical_root(self.root.as_deref())?),
        };
        let mut outputs = HashSet::new();
        let mut work = Vec::with_capacity(inputs.len());
        for input in inputs {
            let output = output_path(&input, &output_directory, self.layout, root.as_deref())?;
            if !outputs.insert(output.clone()) {
                return Err(invalid(format!(
                    "multiple inputs resolve to output {}",
                    output.display()
                )));
            }
            work.push((input.clone(), output, derive_item_seed(self.seed, &input)));
        }

        let fingerprint = fingerprint(&self, &work)?;
        let resolved_graphs = resolved_graphs(&self.filters)?;
        let mutation_impacts = mutation_impacts(&self.filters);
        let environment_dependent = self.filters.iter().any(FilterSpec::environment_dependent);
        if let Some(resume) = &self.resume {
            if resume.version != MANIFEST_VERSION {
                return Err(invalid(format!(
                    "unsupported manifest version {}; expected {MANIFEST_VERSION}",
                    resume.version
                )));
            }
            if resume.plan_version != PIPELINE_PLAN_VERSION {
                return Err(invalid(format!(
                    "unsupported pipeline plan version {}; expected {PIPELINE_PLAN_VERSION}",
                    resume.plan_version
                )));
            }
            if resume.fingerprint != fingerprint {
                return Err(invalid(
                    "resume manifest does not match the resolved batch plan",
                ));
            }
        }

        if self.dry_run {
            return Ok(BatchReport {
                version: MANIFEST_VERSION,
                plan_version: PIPELINE_PLAN_VERSION,
                fingerprint,
                environment_dependent,
                resolved_graphs,
                mutation_impacts,
                items: work
                    .into_iter()
                    .map(|(input, output, seed)| BatchItemResult {
                        input,
                        output,
                        seed,
                        executed: false,
                        resumed: false,
                        error: None,
                    })
                    .collect(),
            });
        }

        let mut results = Vec::new();
        let mut pending = Vec::new();
        for (index, (input, output, seed)) in work.into_iter().enumerate() {
            let completed = self.resume.as_ref().is_some_and(|resume| {
                resume.items.iter().any(|item| {
                    item.input == input
                        && item.output == output
                        && item.seed == seed
                        && item.error.is_none()
                        && (item.executed || item.resumed)
                        && output.exists()
                })
            });
            if completed {
                results.push((
                    index,
                    BatchItemResult {
                        input,
                        output,
                        seed,
                        executed: false,
                        resumed: true,
                        error: None,
                    },
                ));
            } else {
                pending.push((index, input, output, seed));
            }
        }

        let work = Arc::new(pending);
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        let worker_count = self.jobs.min(work.len());
        thread::scope(|scope| {
            for _ in 0..worker_count {
                let work = Arc::clone(&work);
                let next = Arc::clone(&next);
                let sender = sender.clone();
                let filters = &self.filters;
                let video_streams = &self.video_streams;
                let cancellation = cancellation.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((original_index, input, output, seed)) = work.get(index) else {
                        break;
                    };
                    let result = if cancellation.is_cancelled() {
                        BatchItemResult {
                            input: input.clone(),
                            output: output.clone(),
                            seed: *seed,
                            executed: false,
                            resumed: false,
                            error: Some(DatabenderError::Cancelled.to_string()),
                        }
                    } else {
                        execute_item(
                            input,
                            output,
                            filters.clone(),
                            video_streams.clone(),
                            *seed,
                            self.protect_output,
                            cancellation.clone(),
                        )
                    };
                    let _ = sender.send((*original_index, result));
                });
            }
        });
        drop(sender);
        results.extend(receiver);
        results.sort_by_key(|(index, _)| *index);

        Ok(BatchReport {
            version: MANIFEST_VERSION,
            plan_version: PIPELINE_PLAN_VERSION,
            fingerprint,
            environment_dependent,
            resolved_graphs,
            mutation_impacts,
            items: results.into_iter().map(|(_, result)| result).collect(),
        })
    }
}

fn mutation_impacts(filters: &[FilterSpec]) -> Vec<BatchMutationImpact> {
    filters
        .iter()
        .filter_map(|filter| {
            filter
                .impact_estimate()
                .map(|estimate| BatchMutationImpact {
                    filter: filter.name().to_owned(),
                    estimate,
                })
        })
        .collect()
}

fn resolved_graphs(filters: &[FilterSpec]) -> Result<Vec<BatchResolvedGraph>> {
    let mut graphs = Vec::new();
    let mut start = 0;
    while start < filters.len() {
        let domain = filters[start].domain();
        let mut end = start + 1;
        while end < filters.len() && filters[end].domain() == domain {
            end += 1;
        }
        let stage = &filters[start..end];
        match domain {
            FilterDomain::FfmpegAudio => graphs.push(BatchResolvedGraph {
                target: "audio".to_owned(),
                graph: compile_audio_graph(stage)?,
            }),
            FilterDomain::FfmpegVideo => graphs.push(BatchResolvedGraph {
                target: "video".to_owned(),
                graph: compile_video_graph(stage)?,
            }),
            _ => {}
        }
        start = end;
    }
    Ok(graphs)
}

fn execute_item(
    input: &Path,
    output: &Path,
    filters: Vec<FilterSpec>,
    video_streams: Vec<usize>,
    seed: u64,
    protect_output: bool,
    cancellation: CancellationToken,
) -> BatchItemResult {
    let result = fs::create_dir_all(output.parent().expect("batch output always has a parent"))
        .map_err(|error| error.to_string())
        .and_then(|()| {
            TransformRequest::new(input, output, filters, seed)
                .with_output_protection(protect_output)
                .with_video_streams(video_streams)
                .with_cancellation(cancellation)
                .prepare()
                .and_then(codecs::execute)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
    BatchItemResult {
        input: input.to_path_buf(),
        output: output.to_path_buf(),
        seed,
        executed: true,
        resumed: false,
        error: result.err(),
    }
}

fn expand_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut expanded = Vec::new();
    for input in inputs {
        expand(input, &mut expanded)?;
    }
    expanded.sort();
    expanded.dedup();
    Ok(expanded)
}

fn expand(path: &Path, expanded: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() {
        if MediaFormat::from_path_extension(path).is_some() {
            expanded.push(
                fs::canonicalize(path).map_err(|source| DatabenderError::Io {
                    path: path.to_path_buf(),
                    source,
                })?,
            );
        }
    } else if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            expand(&entry.path(), expanded)?;
        }
    }
    Ok(())
}

fn absolute_directory(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|source| DatabenderError::Io {
                path: PathBuf::from("."),
                source,
            })?
            .join(path))
    }
}

fn canonical_root(root: Option<&Path>) -> Result<PathBuf> {
    let root = root.ok_or_else(|| invalid("mirrored layout requires a root directory"))?;
    fs::canonicalize(root).map_err(|source| DatabenderError::Io {
        path: root.to_path_buf(),
        source,
    })
}

fn output_path(
    input: &Path,
    output_directory: &Path,
    layout: BatchLayout,
    root: Option<&Path>,
) -> Result<PathBuf> {
    match layout {
        BatchLayout::Flat => Ok(output_directory.join(
            input
                .file_name()
                .ok_or_else(|| invalid(format!("input {} has no file name", input.display())))?,
        )),
        BatchLayout::Mirrored => {
            let relative = input
                .strip_prefix(root.expect("mirrored layout has a root"))
                .map_err(|_| {
                    invalid(format!(
                        "input {} is outside the batch root",
                        input.display()
                    ))
                })?;
            Ok(output_directory.join(relative))
        }
    }
}

pub fn derive_item_seed(seed: u64, input: &Path) -> u64 {
    derive_seed(
        seed,
        SeedIdentity::File(input.as_os_str().as_encoded_bytes()),
    )
}

fn fingerprint(request: &BatchRequest, work: &[(PathBuf, PathBuf, u64)]) -> Result<u64> {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    hash_bytes(&mut hash, &PIPELINE_PLAN_VERSION.to_le_bytes());
    hash_field(&mut hash, env!("CARGO_PKG_VERSION").as_bytes());
    hash_field(&mut hash, format!("{:?}", request.filters).as_bytes());
    for stream_index in &request.video_streams {
        hash_bytes(&mut hash, &stream_index.to_le_bytes());
    }
    hash_bytes(&mut hash, &[request.protect_output as u8]);
    for (input, output, seed) in work {
        hash_field(&mut hash, input.as_os_str().as_encoded_bytes());
        hash_field(&mut hash, output.as_os_str().as_encoded_bytes());
        hash_bytes(&mut hash, &seed.to_le_bytes());
        hash_file(&mut hash, input)?;
    }
    Ok(hash)
}

fn hash_field(hash: &mut u64, bytes: &[u8]) {
    hash_bytes(hash, &(bytes.len() as u64).to_le_bytes());
    hash_bytes(hash, bytes);
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash = (*hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3);
    }
}

fn hash_file(hash: &mut u64, path: &Path) -> Result<()> {
    let mut file = fs::File::open(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            return Ok(());
        }
        hash_bytes(hash, &buffer[..read]);
    }
}

fn invalid(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidParameter {
        parameter: "batch".to_owned(),
        reason: reason.into(),
    }
}

fn invalid_manifest(path: &Path, reason: impl ToString) -> DatabenderError {
    DatabenderError::InvalidConfiguration {
        path: path.to_path_buf(),
        reason: format!("invalid batch manifest: {}", reason.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_cancelled_batch_stops_before_input_expansion() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let request = BatchRequest {
            inputs: vec![PathBuf::from("missing.png")],
            output_directory: PathBuf::from("output"),
            root: None,
            layout: BatchLayout::Flat,
            filters: vec![FilterSpec::Invert],
            video_streams: Vec::new(),
            seed: 42,
            protect_output: false,
            jobs: 1,
            dry_run: false,
            resume: None,
        };

        assert!(matches!(
            request.execute_with_cancellation(cancellation),
            Err(DatabenderError::Cancelled)
        ));
    }

    #[test]
    fn batch_expansion_excludes_files_without_supported_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let image = directory.path().join("image.JPEG");
        let audio = nested.join("audio.ogg");
        fs::write(&image, b"fixture").unwrap();
        fs::write(&audio, b"fixture").unwrap();
        fs::write(directory.path().join("notes.md"), b"fixture").unwrap();
        fs::write(nested.join("extensionless"), b"fixture").unwrap();

        let expanded = expand_inputs(&[directory.path().to_path_buf()]).unwrap();

        let mut expected = vec![
            fs::canonicalize(image).unwrap(),
            fs::canonicalize(audio).unwrap(),
        ];
        expected.sort();
        assert_eq!(expanded, expected);
    }

    #[test]
    fn video_stream_selection_changes_batch_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.mp4");
        fs::write(&input, b"fixture").unwrap();
        let output = directory.path().join("output.mp4");
        let work = vec![(input.clone(), output, 42)];
        let mut request = BatchRequest {
            inputs: vec![input],
            output_directory: directory.path().to_path_buf(),
            root: None,
            layout: BatchLayout::Flat,
            filters: vec![FilterSpec::Invert],
            video_streams: vec![0],
            seed: 42,
            protect_output: false,
            jobs: 1,
            dry_run: true,
            resume: None,
        };
        let first = fingerprint(&request, &work).unwrap();

        request.video_streams = vec![1];

        assert_ne!(first, fingerprint(&request, &work).unwrap());
    }

    #[test]
    fn resolves_targeted_graphs_for_batch_reports() {
        let filters = [
            FilterSpec::parse("high-pass:frequency=300").unwrap(),
            FilterSpec::parse("expert-audio-graph:volume=1.5").unwrap(),
            FilterSpec::Invert,
            FilterSpec::parse("expert-video-graph:hue=h=20,eq=contrast=1.2").unwrap(),
        ];

        assert_eq!(
            resolved_graphs(&filters).unwrap(),
            vec![
                BatchResolvedGraph {
                    target: "audio".to_owned(),
                    graph: "highpass=f=300,volume=1.5".to_owned(),
                },
                BatchResolvedGraph {
                    target: "video".to_owned(),
                    graph: "hue=h=20,eq=contrast=1.2".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn file_identity_changes_the_derived_item_seed() {
        assert_ne!(
            derive_item_seed(42, Path::new("first/input.mp4")),
            derive_item_seed(42, Path::new("second/input.mp4"))
        );
        assert_eq!(
            derive_item_seed(42, Path::new("first/input.mp4")),
            derive_item_seed(42, Path::new("first/input.mp4"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_have_distinct_seed_and_fingerprint_identities() {
        use std::os::unix::ffi::OsStringExt;

        let first = PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0x80]));
        let second = PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0x81]));

        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(derive_item_seed(42, &first), derive_item_seed(42, &second));
        let mut first_hash = 0;
        let mut second_hash = 0;
        hash_field(&mut first_hash, first.as_os_str().as_encoded_bytes());
        hash_field(&mut second_hash, second.as_os_str().as_encoded_bytes());
        assert_ne!(first_hash, second_hash);
    }
}
