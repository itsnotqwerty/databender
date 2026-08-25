use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::{
    ApplicationService, CancellationToken, DatabenderError, FilterSpec, Result, TransformRequest,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueState {
    #[default]
    Idle,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Finished,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueJob {
    pub input: PathBuf,
    pub output: PathBuf,
    pub filters: Vec<FilterSpec>,
    pub seed: u64,
    pub protect_output: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueItemResult {
    pub input: PathBuf,
    pub output: PathBuf,
    pub seed: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSnapshot {
    pub state: QueueState,
    pub pending: usize,
    pub current: Option<PathBuf>,
    pub completed: usize,
    pub failed: usize,
    pub results: Vec<QueueItemResult>,
    pub logs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct JobQueue {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    data: Mutex<QueueData>,
    wake: Condvar,
    log_limit: usize,
}

#[derive(Debug)]
struct QueueData {
    state: QueueState,
    pending: VecDeque<QueueJob>,
    current: Option<QueueJob>,
    failed: Vec<QueueJob>,
    results: Vec<QueueItemResult>,
    completed: usize,
    logs: VecDeque<String>,
    cancellation: CancellationToken,
}

impl JobQueue {
    pub fn new(log_limit: usize) -> Result<Self> {
        if log_limit == 0 {
            return Err(invalid_queue("log limit must be at least 1"));
        }
        Ok(Self {
            shared: Arc::new(Shared {
                data: Mutex::new(QueueData {
                    state: QueueState::Idle,
                    pending: VecDeque::new(),
                    current: None,
                    failed: Vec::new(),
                    results: Vec::new(),
                    completed: 0,
                    logs: VecDeque::new(),
                    cancellation: CancellationToken::default(),
                }),
                wake: Condvar::new(),
                log_limit,
            }),
        })
    }

    pub fn start(&self, jobs: Vec<QueueJob>) -> Result<()> {
        self.start_inner(jobs, false)
    }

    pub fn start_paused(&self, jobs: Vec<QueueJob>) -> Result<()> {
        self.start_inner(jobs, true)
    }

    fn start_inner(&self, jobs: Vec<QueueJob>, paused: bool) -> Result<()> {
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        if matches!(
            data.state,
            QueueState::Running | QueueState::Paused | QueueState::Cancelling
        ) {
            return Err(invalid_queue("queue is already active"));
        }
        data.pending = jobs.into();
        data.current = None;
        data.failed.clear();
        data.results.clear();
        data.completed = 0;
        data.logs.clear();
        data.cancellation = CancellationToken::default();
        data.state = if data.pending.is_empty() {
            QueueState::Finished
        } else if paused {
            QueueState::Paused
        } else {
            QueueState::Running
        };
        push_log(&self.shared, &mut data, "queue started".to_owned());
        let should_spawn = !data.pending.is_empty();
        drop(data);
        if should_spawn {
            spawn_worker(Arc::clone(&self.shared));
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<()> {
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        if data.state != QueueState::Running {
            return Err(invalid_queue("only a running queue can be paused"));
        }
        data.state = QueueState::Paused;
        push_log(&self.shared, &mut data, "intake paused".to_owned());
        Ok(())
    }

    pub fn resume(&self) -> Result<()> {
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        match data.state {
            QueueState::Paused => {
                data.state = QueueState::Running;
                push_log(&self.shared, &mut data, "intake resumed".to_owned());
                self.shared.wake.notify_all();
                Ok(())
            }
            QueueState::Cancelled if !data.pending.is_empty() => {
                data.cancellation = CancellationToken::default();
                data.state = QueueState::Running;
                push_log(
                    &self.shared,
                    &mut data,
                    "cancelled queue resumed".to_owned(),
                );
                drop(data);
                spawn_worker(Arc::clone(&self.shared));
                Ok(())
            }
            _ => Err(invalid_queue(
                "queue cannot be resumed in its current state",
            )),
        }
    }

    pub fn cancel(&self) -> Result<()> {
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        if !matches!(data.state, QueueState::Running | QueueState::Paused) {
            return Err(invalid_queue("only an active queue can be cancelled"));
        }
        data.state = QueueState::Cancelling;
        data.cancellation.cancel();
        push_log(&self.shared, &mut data, "cancellation requested".to_owned());
        self.shared.wake.notify_all();
        Ok(())
    }

    pub fn retry_failed(&self) -> Result<()> {
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        if data.state != QueueState::Finished {
            return Err(invalid_queue(
                "failed jobs can only be retried after completion",
            ));
        }
        if data.failed.is_empty() {
            return Err(invalid_queue("queue has no failed jobs"));
        }
        let failed = std::mem::take(&mut data.failed);
        data.pending.extend(failed);
        data.cancellation = CancellationToken::default();
        data.state = QueueState::Running;
        push_log(&self.shared, &mut data, "retrying failed jobs".to_owned());
        drop(data);
        spawn_worker(Arc::clone(&self.shared));
        Ok(())
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let data = self.shared.data.lock().expect("queue lock poisoned");
        QueueSnapshot {
            state: data.state,
            pending: data.pending.len(),
            current: data.current.as_ref().map(|job| job.input.clone()),
            completed: data.completed,
            failed: data.failed.len(),
            results: data.results.clone(),
            logs: data.logs.iter().cloned().collect(),
        }
    }

    pub fn wait_for_terminal(&self, timeout: Duration) -> QueueSnapshot {
        let deadline = Instant::now() + timeout;
        let mut data = self.shared.data.lock().expect("queue lock poisoned");
        while !matches!(data.state, QueueState::Cancelled | QueueState::Finished) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = self
                .shared
                .wake
                .wait_timeout(data, deadline - now)
                .expect("queue lock poisoned");
            data = next;
        }
        drop(data);
        self.snapshot()
    }
}

fn spawn_worker(shared: Arc<Shared>) {
    thread::spawn(move || worker(shared));
}

fn worker(shared: Arc<Shared>) {
    loop {
        let (job, cancellation) = {
            let mut data = shared.data.lock().expect("queue lock poisoned");
            while data.state == QueueState::Paused {
                data = shared.wake.wait(data).expect("queue lock poisoned");
            }
            if data.state == QueueState::Cancelling {
                data.state = QueueState::Cancelled;
                push_log(&shared, &mut data, "queue cancelled".to_owned());
                shared.wake.notify_all();
                return;
            }
            let Some(job) = data.pending.pop_front() else {
                data.state = QueueState::Finished;
                push_log(&shared, &mut data, "queue finished".to_owned());
                shared.wake.notify_all();
                return;
            };
            data.current = Some(job.clone());
            push_log(
                &shared,
                &mut data,
                format!("started {}", job.input.display()),
            );
            (job, data.cancellation.clone())
        };

        let result = execute_job(&job, cancellation.clone());
        let mut data = shared.data.lock().expect("queue lock poisoned");
        data.current = None;
        if cancellation.is_cancelled() && matches!(result, Err(DatabenderError::Cancelled)) {
            data.pending.push_front(job);
            data.state = QueueState::Cancelled;
            push_log(&shared, &mut data, "active job cancelled".to_owned());
            shared.wake.notify_all();
            return;
        }
        let error = result.err().map(|error| error.to_string());
        if error.is_some() {
            data.failed.push(job.clone());
        }
        data.completed += 1;
        data.results.push(QueueItemResult {
            input: job.input.clone(),
            output: job.output.clone(),
            seed: job.seed,
            error: error.clone(),
        });
        push_log(
            &shared,
            &mut data,
            match error {
                Some(error) => format!("failed {}: {error}", job.input.display()),
                None => format!("finished {}", job.input.display()),
            },
        );
        shared.wake.notify_all();
    }
}

fn execute_job(job: &QueueJob, cancellation: CancellationToken) -> Result<()> {
    let parent = job
        .output
        .parent()
        .ok_or_else(|| invalid_queue("output has no parent"))?;
    fs::create_dir_all(parent).map_err(|source| DatabenderError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let request = TransformRequest::new(&job.input, &job.output, job.filters.clone(), job.seed)
        .with_output_protection(job.protect_output);
    ApplicationService
        .run_transform_cancellable(request, cancellation, |_| {})
        .map(|_| ())
}

fn push_log(shared: &Shared, data: &mut QueueData, message: String) {
    data.logs.push_back(message);
    while data.logs.len() > shared.log_limit {
        data.logs.pop_front();
    }
}

fn invalid_queue(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidParameter {
        parameter: "queue".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};

    use super::*;

    fn png_job(directory: &std::path::Path, name: &str, seed: u64) -> QueueJob {
        let input = directory.join(format!("{name}.png"));
        ImageBuffer::from_pixel(2, 2, Rgba([10_u8, 20, 30, 255]))
            .save(&input)
            .unwrap();
        QueueJob {
            input,
            output: directory.join("out").join(format!("{name}.png")),
            filters: vec![FilterSpec::Invert],
            seed,
            protect_output: false,
        }
    }

    #[test]
    fn paused_queue_blocks_intake_until_resume() {
        let directory = tempfile::tempdir().unwrap();
        let queue = JobQueue::new(10).unwrap();
        queue
            .start_paused(vec![png_job(directory.path(), "one", 42)])
            .unwrap();

        assert_eq!(queue.snapshot().state, QueueState::Paused);
        assert_eq!(queue.snapshot().pending, 1);
        queue.resume().unwrap();

        let snapshot = queue.wait_for_terminal(Duration::from_secs(2));
        assert_eq!(snapshot.state, QueueState::Finished);
        assert_eq!(snapshot.completed, 1);
        assert_eq!(snapshot.failed, 0);
    }

    #[test]
    fn cancelled_queue_preserves_pending_jobs_for_resume() {
        let directory = tempfile::tempdir().unwrap();
        let queue = JobQueue::new(10).unwrap();
        queue
            .start_paused(vec![png_job(directory.path(), "one", 42)])
            .unwrap();
        queue.cancel().unwrap();

        let cancelled = queue.wait_for_terminal(Duration::from_secs(2));
        assert_eq!(cancelled.state, QueueState::Cancelled);
        assert_eq!(cancelled.pending, 1);
        queue.resume().unwrap();

        let finished = queue.wait_for_terminal(Duration::from_secs(2));
        assert_eq!(finished.state, QueueState::Finished);
        assert_eq!(finished.completed, 1);
    }

    #[test]
    fn retries_failed_jobs_with_the_same_seed_and_bounds_logs() {
        let directory = tempfile::tempdir().unwrap();
        let mut job = png_job(directory.path(), "retry", 77);
        fs::remove_file(&job.input).unwrap();
        let queue = JobQueue::new(2).unwrap();
        queue.start(vec![job.clone()]).unwrap();
        let failed = queue.wait_for_terminal(Duration::from_secs(2));
        assert_eq!(failed.failed, 1);

        ImageBuffer::from_pixel(2, 2, Rgba([10_u8, 20, 30, 255]))
            .save(&job.input)
            .unwrap();
        queue.retry_failed().unwrap();
        let retried = queue.wait_for_terminal(Duration::from_secs(2));

        assert_eq!(retried.state, QueueState::Finished);
        assert_eq!(retried.results.last().unwrap().seed, 77);
        assert!(retried.results.last().unwrap().error.is_none());
        assert!(retried.logs.len() <= 2);
        job.seed = 78;
        assert_ne!(retried.results.last().unwrap().seed, job.seed);
    }
}
