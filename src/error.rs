use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, DatabenderError>;

#[derive(Debug, Error)]
pub enum DatabenderError {
    #[error("could not detect the media format for {path}")]
    FormatDetection { path: PathBuf },

    #[error("format {format} is not supported")]
    UnsupportedFormat { format: String },

    #[error("filter {filter} is incompatible with {format}: {reason}")]
    IncompatibleFilter {
        filter: String,
        format: String,
        reason: String,
    },

    #[error("unknown filter {filter}")]
    UnknownFilter { filter: String },

    #[error("invalid value for {parameter}: {reason}")]
    InvalidParameter { parameter: String, reason: String },

    #[error("refusing to overwrite existing output {path}")]
    OutputExists { path: PathBuf },

    #[error("input and output resolve to the same file: {path}")]
    InputEqualsOutput { path: PathBuf },

    #[error("output validation failed: {reason}")]
    OutputValidation { reason: String },

    #[error("could not decode image {path}: {reason}")]
    ImageDecode { path: PathBuf, reason: String },

    #[error("could not encode {format} image: {reason}")]
    ImageEncode { format: String, reason: String },

    #[error("could not run {tool}: {source}")]
    ExternalToolIo {
        tool: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{tool} timed out after {timeout_ms} ms")]
    ExternalToolTimeout { tool: String, timeout_ms: u128 },

    #[error("{tool} exited with {status}: {stderr}")]
    ExternalToolFailed {
        tool: String,
        status: String,
        stderr: String,
    },

    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
