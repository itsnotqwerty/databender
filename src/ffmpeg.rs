use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use wait_timeout::ChildExt;

use serde::Deserialize;

use crate::{DatabenderError, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_CAPTURE_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedOutput {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutput {
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioProperties {
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioStreamInfo {
    pub codec_name: String,
    pub properties: AudioProperties,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoProperties {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoStreamInfo {
    pub codec_name: String,
    pub properties: VideoProperties,
    pub frame_rate: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasicMetadata {
    pub format: BTreeMap<String, String>,
    pub video: Option<BTreeMap<String, String>>,
    pub audio: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug)]
pub struct ToolRunner {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    timeout: Duration,
    capture_limit: usize,
}

impl Default for ToolRunner {
    fn default() -> Self {
        Self::new("ffmpeg", "ffprobe")
    }
}

impl ToolRunner {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
            timeout: DEFAULT_TIMEOUT,
            capture_limit: DEFAULT_CAPTURE_LIMIT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_capture_limit(mut self, bytes: usize) -> Self {
        self.capture_limit = bytes;
        self
    }

    pub fn ffmpeg<I, S>(&self, arguments: I) -> Result<ToolOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run("ffmpeg", &self.ffmpeg, true, arguments)
    }

    pub fn ffprobe<I, S>(&self, arguments: I) -> Result<ToolOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run("ffprobe", &self.ffprobe, false, arguments)
    }

    pub fn probe_audio(&self, path: impl AsRef<Path>) -> Result<AudioStreamInfo> {
        self.probe_audio_streams(path)?
            .into_iter()
            .next()
            .ok_or_else(|| invalid_probe("ffprobe found no audio stream"))
    }

    pub fn probe_audio_streams(&self, path: impl AsRef<Path>) -> Result<Vec<AudioStreamInfo>> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-select_streams"),
            OsString::from("a"),
            OsString::from("-show_entries"),
            OsString::from("stream=codec_name,sample_rate,channels"),
            OsString::from("-of"),
            OsString::from("json"),
            path.as_ref().as_os_str().to_owned(),
        ];
        let output = self.ffprobe(arguments)?;
        if output.stdout.truncated {
            return Err(invalid_probe("ffprobe JSON exceeded the capture limit"));
        }
        parse_audio_probes(&output.stdout.bytes)
    }

    pub fn validate_audio(
        &self,
        path: impl AsRef<Path>,
        expected: AudioProperties,
    ) -> Result<AudioStreamInfo> {
        let stream = self.probe_audio(path)?;
        validate_audio_properties(stream, expected)
    }

    pub fn validate_audio_streams(
        &self,
        path: impl AsRef<Path>,
        expected: &[AudioProperties],
    ) -> Result<Vec<AudioStreamInfo>> {
        let streams = self.probe_audio_streams(path)?;
        if streams.len() != expected.len() {
            return Err(invalid_probe(format!(
                "audio stream count changed from {} to {}",
                expected.len(),
                streams.len()
            )));
        }
        streams
            .into_iter()
            .zip(expected.iter().copied())
            .map(|(stream, properties)| validate_audio_properties(stream, properties))
            .collect()
    }

    pub fn probe_video(&self, path: impl AsRef<Path>) -> Result<VideoStreamInfo> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-select_streams"),
            OsString::from("v:0"),
            OsString::from("-show_entries"),
            OsString::from("stream=codec_name,width,height,r_frame_rate"),
            OsString::from("-of"),
            OsString::from("json"),
            path.as_ref().as_os_str().to_owned(),
        ];
        let output = self.ffprobe(arguments)?;
        if output.stdout.truncated {
            return Err(invalid_video_probe(
                "ffprobe JSON exceeded the capture limit",
            ));
        }
        parse_video_probe(&output.stdout.bytes)
    }

    pub fn validate_video(
        &self,
        path: impl AsRef<Path>,
        expected: VideoProperties,
    ) -> Result<VideoStreamInfo> {
        let stream = self.probe_video(path)?;
        validate_video_properties(stream, expected)
    }

    pub fn probe_basic_metadata(&self, path: impl AsRef<Path>) -> Result<BasicMetadata> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-show_entries"),
            OsString::from(
                "format_tags=title,artist,album,comment,genre,date,creation_time,copyright:stream=codec_type:stream_tags=title,language",
            ),
            OsString::from("-of"),
            OsString::from("json"),
            path.as_ref().as_os_str().to_owned(),
        ];
        let output = self.ffprobe(arguments)?;
        if output.stdout.truncated {
            return Err(invalid_metadata_probe(
                "ffprobe JSON exceeded the capture limit",
            ));
        }
        parse_basic_metadata(&output.stdout.bytes)
    }

    fn run<I, S>(
        &self,
        name: &str,
        executable: &Path,
        disable_stdin: bool,
        arguments: I,
    ) -> Result<ToolOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(executable);
        if disable_stdin {
            command.arg("-nostdin");
        }
        let mut child = command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| tool_io(name, source))?;
        let stdout = child.stdout.take().expect("stdout was configured as piped");
        let stderr = child.stderr.take().expect("stderr was configured as piped");
        let limit = self.capture_limit;
        let stdout_thread = thread::spawn(move || capture_tail(stdout, limit));
        let stderr_thread = thread::spawn(move || capture_tail(stderr, limit));

        let status = match child
            .wait_timeout(self.timeout)
            .map_err(|source| tool_io(name, source))?
        {
            Some(status) => status,
            None => {
                child.kill().map_err(|source| tool_io(name, source))?;
                child.wait().map_err(|source| tool_io(name, source))?;
                join_capture(name, stdout_thread)?;
                join_capture(name, stderr_thread)?;
                return Err(DatabenderError::ExternalToolTimeout {
                    tool: name.to_owned(),
                    timeout_ms: self.timeout.as_millis(),
                });
            }
        };
        let stdout = join_capture(name, stdout_thread)?;
        let stderr = join_capture(name, stderr_thread)?;

        if !status.success() {
            return Err(DatabenderError::ExternalToolFailed {
                tool: name.to_owned(),
                status: status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            });
        }
        Ok(ToolOutput { stdout, stderr })
    }
}

#[derive(Deserialize)]
struct ProbeDocument {
    #[serde(default)]
    streams: Vec<ProbeAudioStream>,
}

#[derive(Deserialize)]
struct ProbeAudioStream {
    codec_name: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
}

#[derive(Deserialize)]
struct VideoProbeDocument {
    #[serde(default)]
    streams: Vec<ProbeVideoStream>,
}

#[derive(Deserialize)]
struct ProbeVideoStream {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
}

#[derive(Deserialize)]
struct MetadataProbeDocument {
    #[serde(default)]
    streams: Vec<ProbeMetadataStream>,
    format: Option<ProbeFormatMetadata>,
}

#[derive(Deserialize)]
struct ProbeMetadataStream {
    codec_type: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ProbeFormatMetadata {
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[cfg(test)]
fn parse_audio_probe(encoded: &[u8]) -> Result<AudioStreamInfo> {
    parse_audio_probes(encoded)?
        .into_iter()
        .next()
        .ok_or_else(|| invalid_probe("ffprobe found no audio stream"))
}

fn parse_audio_probes(encoded: &[u8]) -> Result<Vec<AudioStreamInfo>> {
    let document: ProbeDocument = serde_json::from_slice(encoded)
        .map_err(|error| invalid_probe(format!("could not parse ffprobe JSON: {error}")))?;
    document
        .streams
        .into_iter()
        .map(parse_audio_stream)
        .collect()
}

fn parse_audio_stream(stream: ProbeAudioStream) -> Result<AudioStreamInfo> {
    let codec_name = stream
        .codec_name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_probe("ffprobe omitted the audio codec name"))?;
    let sample_rate = stream
        .sample_rate
        .ok_or_else(|| invalid_probe("ffprobe omitted the audio sample rate"))?
        .parse::<u32>()
        .map_err(|error| invalid_probe(format!("invalid audio sample rate: {error}")))?;
    let channels = stream
        .channels
        .filter(|channels| *channels > 0)
        .ok_or_else(|| invalid_probe("ffprobe omitted the audio channel count"))?;

    Ok(AudioStreamInfo {
        codec_name,
        properties: AudioProperties {
            sample_rate,
            channels,
        },
    })
}

fn validate_audio_properties(
    stream: AudioStreamInfo,
    expected: AudioProperties,
) -> Result<AudioStreamInfo> {
    if stream.properties != expected {
        return Err(invalid_probe(format!(
            "audio properties changed from {} Hz/{} channels to {} Hz/{} channels",
            expected.sample_rate,
            expected.channels,
            stream.properties.sample_rate,
            stream.properties.channels
        )));
    }
    Ok(stream)
}

fn parse_video_probe(encoded: &[u8]) -> Result<VideoStreamInfo> {
    let document: VideoProbeDocument = serde_json::from_slice(encoded)
        .map_err(|error| invalid_video_probe(format!("could not parse ffprobe JSON: {error}")))?;
    let stream = document
        .streams
        .into_iter()
        .next()
        .ok_or_else(|| invalid_video_probe("ffprobe found no video stream"))?;
    let codec_name = stream
        .codec_name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_video_probe("ffprobe omitted the video codec name"))?;
    let width = stream
        .width
        .filter(|width| *width > 0)
        .ok_or_else(|| invalid_video_probe("ffprobe omitted the video width"))?;
    let height = stream
        .height
        .filter(|height| *height > 0)
        .ok_or_else(|| invalid_video_probe("ffprobe omitted the video height"))?;
    let frame_rate = stream.r_frame_rate.filter(|rate| valid_frame_rate(rate));

    Ok(VideoStreamInfo {
        codec_name,
        properties: VideoProperties { width, height },
        frame_rate,
    })
}

fn valid_frame_rate(rate: &str) -> bool {
    let Some((numerator, denominator)) = rate.split_once('/') else {
        return false;
    };
    numerator.parse::<u64>().is_ok_and(|value| value > 0)
        && denominator.parse::<u64>().is_ok_and(|value| value > 0)
}

fn parse_basic_metadata(encoded: &[u8]) -> Result<BasicMetadata> {
    let document: MetadataProbeDocument = serde_json::from_slice(encoded).map_err(|error| {
        invalid_metadata_probe(format!("could not parse ffprobe JSON: {error}"))
    })?;
    let format = filter_tags(
        document
            .format
            .map_or_else(BTreeMap::new, |format| format.tags),
        &[
            "title",
            "artist",
            "album",
            "comment",
            "genre",
            "date",
            "creation_time",
            "copyright",
        ],
    );
    let mut video = None;
    let mut audio = Vec::new();
    for stream in document.streams {
        let tags = filter_tags(stream.tags, &["title", "language"]);
        match stream.codec_type.as_deref() {
            Some("video") if video.is_none() => video = Some(tags),
            Some("audio") => audio.push(tags),
            _ => {}
        }
    }
    Ok(BasicMetadata {
        format,
        video,
        audio,
    })
}

fn filter_tags(tags: BTreeMap<String, String>, allowlist: &[&str]) -> BTreeMap<String, String> {
    tags.into_iter()
        .filter_map(|(key, value)| {
            let normalized = key.to_ascii_lowercase();
            allowlist
                .contains(&normalized.as_str())
                .then_some((normalized, value))
        })
        .collect()
}

fn validate_video_properties(
    stream: VideoStreamInfo,
    expected: VideoProperties,
) -> Result<VideoStreamInfo> {
    if stream.properties != expected {
        return Err(invalid_video_probe(format!(
            "video dimensions changed from {}x{} to {}x{}",
            expected.width, expected.height, stream.properties.width, stream.properties.height
        )));
    }
    Ok(stream)
}

fn invalid_probe(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid FFmpeg audio output: {}", reason.into()),
    }
}

fn invalid_video_probe(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid FFmpeg video output: {}", reason.into()),
    }
}

fn invalid_metadata_probe(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::OutputValidation {
        reason: format!("invalid FFmpeg metadata output: {}", reason.into()),
    }
}

fn capture_tail(mut reader: impl Read, limit: usize) -> io::Result<CapturedOutput> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if count >= limit {
            bytes.clear();
            bytes.extend_from_slice(&buffer[count - limit..count]);
            truncated = true;
        } else {
            let excess = bytes.len().saturating_add(count).saturating_sub(limit);
            if excess > 0 {
                bytes.drain(..excess);
                truncated = true;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
    }
    Ok(CapturedOutput { bytes, truncated })
}

fn join_capture(
    name: &str,
    handle: thread::JoinHandle<io::Result<CapturedOutput>>,
) -> Result<CapturedOutput> {
    handle
        .join()
        .map_err(|_| tool_io(name, io::Error::other("output reader thread panicked")))?
        .map_err(|source| tool_io(name, source))
}

fn tool_io(tool: &str, source: io::Error) -> DatabenderError {
    DatabenderError::ExternalToolIo {
        tool: tool.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use super::*;

    fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn passes_arguments_without_shell_expansion() {
        let runner = ToolRunner::new("/bin/echo", "/bin/echo");

        let output = runner.ffmpeg(["space value", "$(not-expanded)"]).unwrap();

        assert_eq!(
            output.stdout.bytes,
            b"-nostdin space value $(not-expanded)\n"
        );
        assert!(output.stderr.bytes.is_empty());
    }

    #[test]
    fn keeps_ffprobe_noninteractive_without_an_unsupported_flag() {
        let runner = ToolRunner::new("/bin/echo", "/bin/echo");

        let output = runner.ffprobe(["-version"]).unwrap();

        assert_eq!(output.stdout.bytes, b"-version\n");
    }

    #[test]
    fn retains_only_the_bounded_stderr_tail_on_failure() {
        let directory = tempfile::tempdir().unwrap();
        let executable = script(
            directory.path(),
            "failure",
            "i=0; while [ $i -lt 256 ]; do printf x >&2; i=$((i + 1)); done; printf END >&2; exit 7",
        );
        let runner = ToolRunner::new("/bin/sh", "/bin/sh").with_capture_limit(32);

        let error = runner.ffprobe([executable]).unwrap_err();

        match error {
            DatabenderError::ExternalToolFailed { status, stderr, .. } => {
                assert_eq!(status, "7");
                assert_eq!(stderr.len(), 32);
                assert!(stderr.ends_with("END"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn kills_and_reaps_a_process_after_the_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let executable = script(directory.path(), "blocked", "while :; do :; done");
        let runner = ToolRunner::new("/bin/sh", "/bin/sh").with_timeout(Duration::from_millis(25));
        let started = Instant::now();

        let error = runner.ffprobe([executable]).unwrap_err();

        assert!(matches!(error, DatabenderError::ExternalToolTimeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn reports_an_unavailable_executable() {
        let runner = ToolRunner::new("/missing/databender-ffmpeg", "/missing/databender-ffprobe");

        let error = runner.ffmpeg(std::iter::empty::<&str>()).unwrap_err();

        assert!(matches!(error, DatabenderError::ExternalToolIo { .. }));
        assert!(error.to_string().contains("could not run ffmpeg"));
    }

    #[test]
    fn parses_and_validates_typed_ffprobe_json() {
        let stream = validate_audio_properties(
            parse_audio_probe(
                br#"{"streams":[{"codec_name":"pcm_s16le","sample_rate":"48000","channels":2}]}"#,
            )
            .unwrap(),
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap();

        assert_eq!(stream.codec_name, "pcm_s16le");
        assert_eq!(stream.properties.channels, 2);
    }

    #[test]
    fn rejects_missing_malformed_and_mismatched_probe_data() {
        assert!(parse_audio_probe(b"not json").is_err());
        assert!(parse_audio_probe(br#"{"streams":[]}"#).is_err());

        let stream = parse_audio_probe(
            br#"{"streams":[{"codec_name":"aac","sample_rate":"44100","channels":1}]}"#,
        )
        .unwrap();
        let error = validate_audio_properties(
            stream,
            AudioProperties {
                sample_rate: 48_000,
                channels: 2,
            },
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("audio properties changed"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parses_and_validates_all_audio_streams() {
        let streams = parse_audio_probes(
            br#"{"streams":[{"codec_name":"aac","sample_rate":"48000","channels":2},{"codec_name":"aac","sample_rate":"44100","channels":1}]}"#,
        )
        .unwrap();

        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].properties.sample_rate, 48_000);
        assert_eq!(streams[1].properties.channels, 1);
    }

    #[test]
    fn parses_and_validates_typed_video_probe_json() {
        let stream = validate_video_properties(
            parse_video_probe(
                br#"{"streams":[{"codec_name":"h264","width":320,"height":180,"r_frame_rate":"30000/1001"}]}"#,
            )
            .unwrap(),
            VideoProperties {
                width: 320,
                height: 180,
            },
        )
        .unwrap();

        assert_eq!(stream.codec_name, "h264");
        assert_eq!(stream.properties.height, 180);
        assert_eq!(stream.frame_rate.as_deref(), Some("30000/1001"));
    }

    #[test]
    fn rejects_missing_and_mismatched_video_probe_data() {
        assert!(parse_video_probe(br#"{"streams":[]}"#).is_err());

        let stream =
            parse_video_probe(br#"{"streams":[{"codec_name":"h264","width":640,"height":360}]}"#)
                .unwrap();
        let error = validate_video_properties(
            stream,
            VideoProperties {
                width: 320,
                height: 180,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("video dimensions changed"));
    }

    #[test]
    fn parses_only_allowlisted_basic_metadata() {
        let metadata = parse_basic_metadata(
            br#"{"streams":[{"codec_type":"video","tags":{"language":"und","title":"Picture","encoder":"ignored"}},{"codec_type":"audio","tags":{"language":"eng","title":"Main"}}],"format":{"tags":{"title":"Fixture","artist":"Databender","major_brand":"ignored"}}}"#,
        )
        .unwrap();

        assert_eq!(
            metadata.format.get("title").map(String::as_str),
            Some("Fixture")
        );
        assert_eq!(
            metadata.format.get("artist").map(String::as_str),
            Some("Databender")
        );
        assert!(!metadata.format.contains_key("major_brand"));
        assert_eq!(
            metadata.video.unwrap().get("title").map(String::as_str),
            Some("Picture")
        );
        assert_eq!(metadata.audio.len(), 1);
        assert_eq!(
            metadata.audio[0].get("language").map(String::as_str),
            Some("eng")
        );
    }
}
