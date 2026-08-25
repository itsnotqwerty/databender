use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use wait_timeout::ChildExt;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{CancellationToken, DatabenderError, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_CAPTURE_LIMIT: usize = 64 * 1024;
const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;

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
    pub frame_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoCodecConfig {
    pub codec_name: String,
    pub profile: Option<String>,
    pub level: Option<i32>,
    pub time_base: String,
    pub extradata_hash: Option<String>,
    pub nal_length_size: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoPacketRecord {
    pub stream_index: usize,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub duration: Option<i64>,
    pub position: Option<u64>,
    pub size: usize,
    pub keyframe: bool,
    pub data_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DemuxedVideoStream {
    pub stream_index: usize,
    pub codec: VideoCodecConfig,
    pub packets: Vec<VideoPacketRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasicMetadata {
    pub format: BTreeMap<String, String>,
    pub video: Vec<BTreeMap<String, String>>,
    pub audio: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuxiliaryStreamInfo {
    pub codec_name: String,
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChapterInfo {
    pub id: Option<i64>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatroskaAuxiliary {
    pub subtitles: Vec<AuxiliaryStreamInfo>,
    pub attachments: Vec<AuxiliaryStreamInfo>,
    pub chapters: Vec<ChapterInfo>,
}

#[derive(Clone, Debug)]
pub struct ToolRunner {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    avifenc: PathBuf,
    timeout: Duration,
    capture_limit: usize,
    cancellation: CancellationToken,
}

impl Default for ToolRunner {
    fn default() -> Self {
        Self::new(
            std::env::var_os("DATABENDER_FFMPEG").unwrap_or_else(|| OsString::from("ffmpeg")),
            std::env::var_os("DATABENDER_FFPROBE").unwrap_or_else(|| OsString::from("ffprobe")),
        )
    }
}

impl ToolRunner {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
            avifenc: std::env::var_os("DATABENDER_AVIFENC")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("avifenc")),
            timeout: DEFAULT_TIMEOUT,
            capture_limit: DEFAULT_CAPTURE_LIMIT,
            cancellation: CancellationToken::default(),
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

    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }

    pub fn check_cancelled(&self) -> Result<()> {
        self.cancellation.check()
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

    pub fn avifenc<I, S>(&self, arguments: I) -> Result<ToolOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run("avifenc", &self.avifenc, false, arguments)
    }

    pub fn ffmpeg_available(&self) -> bool {
        self.ffmpeg([OsString::from("-version")]).is_ok()
    }

    pub fn ffprobe_available(&self) -> bool {
        self.ffprobe([OsString::from("-version")]).is_ok()
    }

    pub fn avifenc_available(&self) -> bool {
        self.avifenc([OsString::from("--version")]).is_ok()
    }

    pub fn supports_encoder(&self, encoder: &str) -> bool {
        let Ok(output) = self.ffmpeg([
            OsString::from("-hide_banner"),
            OsString::from("-h"),
            OsString::from(format!("encoder={encoder}")),
        ]) else {
            return false;
        };
        !output.stdout.truncated
            && !output.stderr.truncated
            && !contains_diagnostic(&output, b"not recognized by FFmpeg")
    }

    pub fn supports_filter(&self, filter: &str) -> bool {
        let Ok(output) = self.ffmpeg([
            OsString::from("-hide_banner"),
            OsString::from("-h"),
            OsString::from(format!("filter={filter}")),
        ]) else {
            return false;
        };
        !output.stdout.truncated
            && !output.stderr.truncated
            && !contains_diagnostic(&output, b"not recognized by FFmpeg")
            && !contains_diagnostic(&output, b"Unknown filter")
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

    pub fn probe_video_streams(&self, path: impl AsRef<Path>) -> Result<Vec<VideoStreamInfo>> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-count_frames"),
            OsString::from("-select_streams"),
            OsString::from("v"),
            OsString::from("-show_entries"),
            OsString::from("stream=codec_name,width,height,avg_frame_rate,nb_read_frames"),
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
        parse_video_probes(&output.stdout.bytes)
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
            OsString::from("-count_frames"),
            OsString::from("-select_streams"),
            OsString::from("v:0"),
            OsString::from("-show_entries"),
            OsString::from("stream=codec_name,width,height,avg_frame_rate,nb_read_frames"),
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

    pub fn probe_video_packets(
        &self,
        path: impl AsRef<Path>,
        video_stream: usize,
    ) -> Result<DemuxedVideoStream> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-select_streams"),
            OsString::from(format!("v:{video_stream}")),
            OsString::from("-show_packets"),
            OsString::from("-show_data_hash"),
            OsString::from("sha256"),
            OsString::from("-show_entries"),
            OsString::from(
                "stream=index,codec_name,profile,level,time_base,extradata_hash,nal_length_size:packet=stream_index,pts,dts,duration,size,pos,flags,data_hash",
            ),
            OsString::from("-of"),
            OsString::from("json"),
            path.as_ref().as_os_str().to_owned(),
        ];
        let output = self.ffprobe(arguments)?;
        if output.stdout.truncated {
            return Err(invalid_video_probe(
                "ffprobe packet JSON exceeded the capture limit",
            ));
        }
        parse_video_packets(&output.stdout.bytes)
    }

    pub fn read_video_packet(
        &self,
        path: impl AsRef<Path>,
        packet: &VideoPacketRecord,
    ) -> Result<Vec<u8>> {
        let path = path.as_ref();
        let position = packet
            .position
            .ok_or_else(|| invalid_video_probe("packet omitted its file position"))?;
        if packet.size > MAX_PACKET_BYTES {
            return Err(invalid_video_probe(format!(
                "packet size {} exceeds the {MAX_PACKET_BYTES}-byte read limit",
                packet.size
            )));
        }
        let mut file = File::open(path).map_err(|source| DatabenderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        file.seek(SeekFrom::Start(position))
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut encoded = vec![0; packet.size];
        file.read_exact(&mut encoded)
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let actual_hash = format!("SHA256:{:x}", Sha256::digest(&encoded));
        if actual_hash != packet.data_hash {
            return Err(invalid_video_probe(
                "packet file position does not identify its exact payload bytes",
            ));
        }
        Ok(encoded)
    }

    pub fn validate_video(
        &self,
        path: impl AsRef<Path>,
        expected: VideoProperties,
    ) -> Result<VideoStreamInfo> {
        let stream = self.probe_video(path)?;
        validate_video_properties(stream, expected)
    }

    pub fn validate_video_streams(
        &self,
        path: impl AsRef<Path>,
        expected: &[VideoStreamInfo],
    ) -> Result<Vec<VideoStreamInfo>> {
        let streams = self.probe_video_streams(path)?;
        if streams.len() != expected.len() {
            return Err(invalid_video_probe(format!(
                "video stream count changed from {} to {}",
                expected.len(),
                streams.len()
            )));
        }
        streams
            .into_iter()
            .zip(expected)
            .map(|(stream, expected)| validate_video_timing(stream, expected))
            .collect()
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

    pub fn probe_matroska_auxiliary(&self, path: impl AsRef<Path>) -> Result<MatroskaAuxiliary> {
        let arguments = [
            OsString::from("-v"),
            OsString::from("error"),
            OsString::from("-show_entries"),
            OsString::from(
                "stream=codec_type,codec_name:stream_tags=title,language,filename,mimetype:chapter=id,start_time,end_time:chapter_tags=title,language",
            ),
            OsString::from("-of"),
            OsString::from("json"),
            path.as_ref().as_os_str().to_owned(),
        ];
        let output = self.ffprobe(arguments)?;
        if output.stdout.truncated {
            return Err(invalid_metadata_probe(
                "ffprobe auxiliary JSON exceeded the capture limit",
            ));
        }
        parse_matroska_auxiliary(&output.stdout.bytes)
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

        let started = Instant::now();
        let status = loop {
            if self.cancellation.is_cancelled() {
                child.kill().map_err(|source| tool_io(name, source))?;
                child.wait().map_err(|source| tool_io(name, source))?;
                join_capture(name, stdout_thread)?;
                join_capture(name, stderr_thread)?;
                return Err(DatabenderError::Cancelled);
            }
            let elapsed = started.elapsed();
            if elapsed >= self.timeout {
                child.kill().map_err(|source| tool_io(name, source))?;
                child.wait().map_err(|source| tool_io(name, source))?;
                join_capture(name, stdout_thread)?;
                join_capture(name, stderr_thread)?;
                return Err(DatabenderError::ExternalToolTimeout {
                    tool: name.to_owned(),
                    timeout_ms: self.timeout.as_millis(),
                });
            }
            let interval = (self.timeout - elapsed).min(Duration::from_millis(50));
            if let Some(status) = child
                .wait_timeout(interval)
                .map_err(|source| tool_io(name, source))?
            {
                break status;
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

fn contains_diagnostic(output: &ToolOutput, needle: &[u8]) -> bool {
    output
        .stdout
        .bytes
        .windows(needle.len())
        .chain(output.stderr.bytes.windows(needle.len()))
        .any(|window| window == needle)
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
    avg_frame_rate: Option<String>,
    nb_read_frames: Option<String>,
}

#[derive(Deserialize)]
struct VideoPacketProbeDocument {
    #[serde(default)]
    streams: Vec<ProbePacketVideoStream>,
    #[serde(default)]
    packets: Vec<ProbeVideoPacket>,
}

#[derive(Deserialize)]
struct ProbePacketVideoStream {
    index: Option<usize>,
    codec_name: Option<String>,
    profile: Option<String>,
    level: Option<i32>,
    time_base: Option<String>,
    extradata_hash: Option<String>,
    nal_length_size: Option<String>,
}

#[derive(Deserialize)]
struct ProbeVideoPacket {
    stream_index: Option<usize>,
    pts: Option<i64>,
    dts: Option<i64>,
    duration: Option<i64>,
    pos: Option<String>,
    size: Option<String>,
    flags: Option<String>,
    data_hash: Option<String>,
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

#[derive(Deserialize)]
struct AuxiliaryProbeDocument {
    #[serde(default)]
    streams: Vec<ProbeAuxiliaryStream>,
    #[serde(default)]
    chapters: Vec<ProbeChapter>,
}

#[derive(Deserialize)]
struct ProbeAuxiliaryStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ProbeChapter {
    id: Option<i64>,
    start_time: Option<String>,
    end_time: Option<String>,
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
    parse_video_probes(encoded)?
        .into_iter()
        .next()
        .ok_or_else(|| invalid_video_probe("ffprobe found no video stream"))
}

fn parse_video_probes(encoded: &[u8]) -> Result<Vec<VideoStreamInfo>> {
    let document: VideoProbeDocument = serde_json::from_slice(encoded)
        .map_err(|error| invalid_video_probe(format!("could not parse ffprobe JSON: {error}")))?;
    document
        .streams
        .into_iter()
        .map(parse_video_stream)
        .collect()
}

fn parse_video_stream(stream: ProbeVideoStream) -> Result<VideoStreamInfo> {
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
    let frame_rate = stream.avg_frame_rate.filter(|rate| valid_frame_rate(rate));
    let frame_count = stream
        .nb_read_frames
        .ok_or_else(|| invalid_video_probe("ffprobe omitted the decoded video frame count"))?
        .parse::<u64>()
        .map_err(|error| invalid_video_probe(format!("invalid video frame count: {error}")))?;

    Ok(VideoStreamInfo {
        codec_name,
        properties: VideoProperties { width, height },
        frame_rate,
        frame_count,
    })
}

fn parse_video_packets(encoded: &[u8]) -> Result<DemuxedVideoStream> {
    let document: VideoPacketProbeDocument = serde_json::from_slice(encoded).map_err(|error| {
        invalid_video_probe(format!("could not parse packet probe JSON: {error}"))
    })?;
    if document.streams.len() != 1 {
        return Err(invalid_video_probe(format!(
            "packet probe returned {} streams instead of one",
            document.streams.len()
        )));
    }
    let stream = document.streams.into_iter().next().unwrap();
    let stream_index = stream
        .index
        .ok_or_else(|| invalid_video_probe("packet probe omitted the stream index"))?;
    let codec_name = stream
        .codec_name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid_video_probe("packet probe omitted the codec name"))?;
    let time_base = stream
        .time_base
        .filter(|value| valid_frame_rate(value))
        .ok_or_else(|| invalid_video_probe("packet probe omitted a valid time base"))?;
    let packets = document
        .packets
        .into_iter()
        .map(|packet| {
            let packet_stream = packet
                .stream_index
                .ok_or_else(|| invalid_video_probe("packet omitted its stream index"))?;
            if packet_stream != stream_index {
                return Err(invalid_video_probe(format!(
                    "packet belongs to stream {packet_stream} instead of {stream_index}"
                )));
            }
            let size = parse_optional_number(packet.size.as_deref(), "packet size")?
                .ok_or_else(|| invalid_video_probe("packet omitted its size"))?;
            let size = usize::try_from(size)
                .map_err(|_| invalid_video_probe("packet size exceeds the address space"))?;
            Ok(VideoPacketRecord {
                stream_index,
                pts: packet.pts,
                dts: packet.dts,
                duration: packet.duration,
                position: parse_optional_number(packet.pos.as_deref(), "packet position")?,
                size,
                keyframe: packet.flags.is_some_and(|flags| flags.contains('K')),
                data_hash: packet
                    .data_hash
                    .filter(|hash| hash.starts_with("SHA256:") && hash.len() == 71)
                    .ok_or_else(|| invalid_video_probe("packet omitted its SHA-256 data hash"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DemuxedVideoStream {
        stream_index,
        codec: VideoCodecConfig {
            codec_name,
            profile: stream.profile.filter(|profile| !profile.is_empty()),
            level: stream.level,
            time_base,
            extradata_hash: stream.extradata_hash.filter(|hash| !hash.is_empty()),
            nal_length_size: parse_optional_number(
                stream.nal_length_size.as_deref(),
                "NAL length size",
            )?
            .map(|size| {
                usize::try_from(size)
                    .map_err(|_| invalid_video_probe("NAL length size exceeds usize"))
            })
            .transpose()?,
        },
        packets,
    })
}

fn parse_optional_number(value: Option<&str>, field: &str) -> Result<Option<u64>> {
    value
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|error| invalid_video_probe(format!("invalid {field}: {error}")))
        })
        .transpose()
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
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for stream in document.streams {
        let tags = filter_tags(stream.tags, &["title", "language"]);
        match stream.codec_type.as_deref() {
            Some("video") => video.push(tags),
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

fn parse_matroska_auxiliary(encoded: &[u8]) -> Result<MatroskaAuxiliary> {
    let document: AuxiliaryProbeDocument = serde_json::from_slice(encoded).map_err(|error| {
        invalid_metadata_probe(format!("could not parse auxiliary ffprobe JSON: {error}"))
    })?;
    let mut subtitles = Vec::new();
    let mut attachments = Vec::new();
    for stream in document.streams {
        let destination = match stream.codec_type.as_deref() {
            Some("subtitle") => &mut subtitles,
            Some("attachment") => &mut attachments,
            _ => continue,
        };
        destination.push(AuxiliaryStreamInfo {
            codec_name: stream
                .codec_name
                .filter(|name| !name.is_empty())
                .ok_or_else(|| invalid_metadata_probe("auxiliary stream omitted its codec"))?,
            tags: filter_tags(stream.tags, &["title", "language", "filename", "mimetype"]),
        });
    }
    let chapters = document
        .chapters
        .into_iter()
        .map(|chapter| ChapterInfo {
            id: chapter.id,
            start_time: chapter.start_time,
            end_time: chapter.end_time,
            tags: filter_tags(chapter.tags, &["title", "language"]),
        })
        .collect();
    Ok(MatroskaAuxiliary {
        subtitles,
        attachments,
        chapters,
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

fn validate_video_timing(
    stream: VideoStreamInfo,
    expected: &VideoStreamInfo,
) -> Result<VideoStreamInfo> {
    let stream = validate_video_properties(stream, expected.properties)?;
    if stream.frame_count != expected.frame_count {
        return Err(invalid_video_probe(format!(
            "video frame count changed from {} to {}",
            expected.frame_count, stream.frame_count
        )));
    }
    if stream.frame_rate != expected.frame_rate {
        return Err(invalid_video_probe(format!(
            "video frame rate changed from {:?} to {:?}",
            expected.frame_rate, stream.frame_rate
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

    #[cfg(unix)]
    fn script(directory: &Path, name: &str, body: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn keeps_ffprobe_noninteractive_without_an_unsupported_flag() {
        let runner = ToolRunner::new("/bin/echo", "/bin/echo");

        let output = runner.ffprobe(["-version"]).unwrap();

        assert_eq!(output.stdout.bytes, b"-version\n");
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn cancellation_kills_and_reaps_a_running_process() {
        let directory = tempfile::tempdir().unwrap();
        let executable = script(directory.path(), "blocked", "while :; do :; done");
        let cancellation = CancellationToken::default();
        let cancel_from_thread = cancellation.clone();
        let runner = ToolRunner::new("/bin/sh", "/bin/sh")
            .with_timeout(Duration::from_secs(5))
            .with_cancellation(cancellation);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            cancel_from_thread.cancel();
        });
        let started = Instant::now();

        let error = runner.ffprobe([executable]).unwrap_err();
        canceller.join().unwrap();

        assert!(matches!(error, DatabenderError::Cancelled));
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
    fn reports_an_unavailable_encoder() {
        let runner = ToolRunner::default();
        if runner.ffmpeg_available() {
            assert!(!runner.supports_encoder("databender_missing_encoder"));
        }
    }

    #[test]
    fn reports_an_unavailable_filter() {
        let runner = ToolRunner::default();
        if runner.ffmpeg_available() {
            assert!(!runner.supports_filter("databender_missing_filter"));
        }
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
                br#"{"streams":[{"codec_name":"h264","width":320,"height":180,"avg_frame_rate":"30000/1001","nb_read_frames":"12"}]}"#,
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
        assert_eq!(stream.frame_count, 12);
    }

    #[test]
    fn parses_all_video_streams_in_order() {
        let streams = parse_video_probes(
            br#"{"streams":[{"codec_name":"h264","width":320,"height":180,"avg_frame_rate":"30/1","nb_read_frames":"3"},{"codec_name":"vp9","width":640,"height":360,"avg_frame_rate":"24/1","nb_read_frames":"5"}]}"#,
        )
        .unwrap();

        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].codec_name, "h264");
        assert_eq!(streams[0].properties.width, 320);
        assert_eq!(streams[1].codec_name, "vp9");
        assert_eq!(streams[1].properties.width, 640);
        assert_eq!(streams[1].frame_count, 5);
    }

    #[test]
    fn parses_codec_aware_video_packet_records() {
        let stream = parse_video_packets(
            br#"{
                "streams":[{"index":2,"codec_name":"h264","profile":"High","level":40,"time_base":"1/90000","extradata_hash":"SHA256:abc","nal_length_size":"4"}],
                "packets":[
                    {"stream_index":2,"pts":0,"dts":-3000,"duration":3000,"size":"128","pos":"4096","flags":"K_","data_hash":"SHA256:0000000000000000000000000000000000000000000000000000000000000000"},
                    {"stream_index":2,"pts":6000,"dts":0,"duration":3000,"size":"64","pos":"4224","flags":"__","data_hash":"SHA256:1111111111111111111111111111111111111111111111111111111111111111"}
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(stream.stream_index, 2);
        assert_eq!(stream.codec.codec_name, "h264");
        assert_eq!(stream.codec.extradata_hash.as_deref(), Some("SHA256:abc"));
        assert_eq!(stream.codec.nal_length_size, Some(4));
        assert_eq!(stream.packets.len(), 2);
        assert!(stream.packets[0].keyframe);
        assert_eq!(stream.packets[1].position, Some(4224));
        assert!(parse_video_packets(
            br#"{"streams":[{"index":2,"codec_name":"h264","time_base":"1/90000"}],"packets":[{"stream_index":3,"size":"1"}]}"#
        )
        .is_err());
    }

    #[test]
    fn rejects_missing_and_mismatched_video_probe_data() {
        assert!(parse_video_probe(br#"{"streams":[]}"#).is_err());

        let stream = parse_video_probe(
            br#"{"streams":[{"codec_name":"h264","width":640,"height":360,"nb_read_frames":"1"}]}"#,
        )
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
            metadata.video[0].get("title").map(String::as_str),
            Some("Picture")
        );
        assert_eq!(metadata.audio.len(), 1);
        assert_eq!(
            metadata.audio[0].get("language").map(String::as_str),
            Some("eng")
        );
    }

    #[test]
    fn parses_metadata_for_all_video_streams_in_order() {
        let metadata = parse_basic_metadata(
            br#"{"streams":[{"codec_type":"video","tags":{"title":"Primary","language":"eng"}},{"codec_type":"video","tags":{"title":"Alternate","language":"spa"}}]}"#,
        )
        .unwrap();

        assert_eq!(metadata.video.len(), 2);
        assert_eq!(
            metadata.video[0].get("title").map(String::as_str),
            Some("Primary")
        );
        assert_eq!(
            metadata.video[1].get("language").map(String::as_str),
            Some("spa")
        );
    }
}
