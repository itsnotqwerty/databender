use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{DatabenderError, Result};

pub const PLUGIN_MANIFEST_VERSION: u32 = 1;
pub const PLUGIN_ABI_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub manifest_version: u32,
    pub abi_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub filters: Vec<PluginFilterMetadata>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginFilterMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub domains: Vec<PluginDomain>,
    pub deterministic: bool,
    #[serde(default)]
    pub parameters: Vec<PluginParameterMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginDomain {
    ImageFrame,
    PcmAudio,
    EncodedPayload,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginParameterMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: PluginParameterKind,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PluginParameterKind {
    Boolean {
        default: bool,
    },
    Integer {
        minimum: i64,
        maximum: i64,
        default: i64,
    },
    Float {
        minimum: f64,
        maximum: f64,
        default: f64,
    },
    Choice {
        choices: Vec<String>,
        default: String,
    },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PluginValue {
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum PluginCommand {
    Invoke(PluginInvocation),
    Cancel { invocation_id: u64 },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInvocation {
    pub abi_version: u32,
    pub invocation_id: u64,
    pub filter_id: String,
    pub seed: u64,
    pub parameters: BTreeMap<String, PluginValue>,
    pub input: PluginMedia,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "domain", rename_all = "kebab-case")]
pub enum PluginMedia {
    ImageFrame {
        width: u32,
        height: u32,
        stride: u32,
        pixel_format: PluginPixelFormat,
        data: Vec<u8>,
    },
    PcmAudio {
        sample_rate: u32,
        channels: u32,
        sample_format: PluginSampleFormat,
        frame_count: u64,
        data: Vec<u8>,
    },
    EncodedPayload {
        format: String,
        data: Vec<u8>,
        regions: Vec<PluginPayloadRegion>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPayloadRegion {
    pub offset: u64,
    pub length: u64,
    pub kind: String,
    pub mutable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginPixelFormat {
    Rgba8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginSampleFormat {
    Signed16LittleEndian,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum PluginEvent {
    Progress(PluginProgress),
    Completed {
        invocation_id: u64,
        output: PluginMedia,
    },
    Failed {
        invocation_id: u64,
        error: PluginError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginProgress {
    pub invocation_id: u64,
    pub completed: u64,
    pub total: u64,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginError {
    pub code: PluginErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginErrorCode {
    AbiMismatch,
    InvalidInput,
    InvalidParameter,
    UnsupportedDomain,
    Cancelled,
    ResourceLimit,
    ExecutionFailed,
}

impl PluginManifest {
    pub fn validate(&self) -> std::result::Result<(), String> {
        self.compatibility_error()?;
        self.validate_structure()
    }

    pub(crate) fn compatibility_error(&self) -> std::result::Result<(), String> {
        if self.manifest_version != PLUGIN_MANIFEST_VERSION {
            return Err(format!(
                "unsupported manifest version {}; expected {PLUGIN_MANIFEST_VERSION}",
                self.manifest_version
            ));
        }
        if self.abi_version != PLUGIN_ABI_VERSION {
            return Err(format!(
                "unsupported ABI version {}; expected {PLUGIN_ABI_VERSION}",
                self.abi_version
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_structure(&self) -> std::result::Result<(), String> {
        validate_identifier("plugin", &self.id)?;
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err("plugin name and version must not be empty".to_owned());
        }
        if self.filters.is_empty() {
            return Err("plugin must declare at least one filter".to_owned());
        }
        let mut filter_ids = std::collections::HashSet::new();
        for filter in &self.filters {
            validate_identifier("filter", &filter.id)?;
            if !filter_ids.insert(filter.id.as_str()) {
                return Err(format!("duplicate filter id {:?}", filter.id));
            }
            if filter.name.trim().is_empty() || filter.description.trim().is_empty() {
                return Err(format!(
                    "filter {:?} requires a name and description",
                    filter.id
                ));
            }
            if filter.domains.is_empty() {
                return Err(format!("filter {:?} has no supported domains", filter.id));
            }
            let mut domains = std::collections::HashSet::new();
            for domain in &filter.domains {
                if !domains.insert(domain) {
                    return Err(format!("filter {:?} repeats domain {domain:?}", filter.id));
                }
            }
            validate_parameters(filter)?;
        }
        Ok(())
    }
}

impl PluginInvocation {
    pub fn validate(&self) -> std::result::Result<(), PluginError> {
        if self.abi_version != PLUGIN_ABI_VERSION {
            return Err(PluginError {
                code: PluginErrorCode::AbiMismatch,
                message: format!(
                    "unsupported ABI version {}; expected {PLUGIN_ABI_VERSION}",
                    self.abi_version
                ),
                retryable: false,
            });
        }
        validate_identifier("filter", &self.filter_id).map_err(|message| PluginError {
            code: PluginErrorCode::InvalidParameter,
            message,
            retryable: false,
        })?;
        validate_media(&self.input)
    }
}

impl PluginProgress {
    pub fn validate(&self) -> std::result::Result<(), PluginError> {
        if self.total == 0 || self.completed > self.total {
            return Err(PluginError {
                code: PluginErrorCode::InvalidInput,
                message: "progress requires 0 <= completed <= total and total > 0".to_owned(),
                retryable: false,
            });
        }
        Ok(())
    }
}

pub fn load_plugin_manifest(path: impl AsRef<Path>) -> Result<PluginManifest> {
    let path = path.as_ref();
    let encoded = fs::read(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let manifest: PluginManifest = serde_json::from_slice(&encoded).map_err(|error| {
        DatabenderError::InvalidConfiguration {
            path: path.to_path_buf(),
            reason: format!("invalid plugin manifest: {error}"),
        }
    })?;
    manifest
        .validate()
        .map_err(|reason| DatabenderError::InvalidConfiguration {
            path: path.to_path_buf(),
            reason: format!("invalid plugin manifest: {reason}"),
        })?;
    Ok(manifest)
}

fn validate_parameters(filter: &PluginFilterMetadata) -> std::result::Result<(), String> {
    let mut parameter_ids = std::collections::HashSet::new();
    for parameter in &filter.parameters {
        validate_identifier("parameter", &parameter.id)?;
        if !parameter_ids.insert(parameter.id.as_str()) {
            return Err(format!(
                "filter {:?} repeats parameter {:?}",
                filter.id, parameter.id
            ));
        }
        if parameter.name.trim().is_empty() || parameter.description.trim().is_empty() {
            return Err(format!(
                "parameter {:?} requires a name and description",
                parameter.id
            ));
        }
        match &parameter.kind {
            PluginParameterKind::Boolean { .. } => {}
            PluginParameterKind::Integer {
                minimum,
                maximum,
                default,
            } if minimum <= default && default <= maximum => {}
            PluginParameterKind::Float {
                minimum,
                maximum,
                default,
            } if minimum.is_finite()
                && maximum.is_finite()
                && default.is_finite()
                && minimum <= default
                && default <= maximum => {}
            PluginParameterKind::Choice { choices, default }
                if !choices.is_empty()
                    && choices.iter().all(|choice| !choice.is_empty())
                    && choices.contains(default) => {}
            _ => {
                return Err(format!(
                    "parameter {:?} has invalid bounds or default",
                    parameter.id
                ))
            }
        }
    }
    Ok(())
}

fn validate_identifier(kind: &str, value: &str) -> std::result::Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        });
    valid.then_some(()).ok_or_else(|| {
        format!("{kind} id {value:?} must use lowercase ASCII letters, digits, dots, or hyphens")
    })
}

fn validate_media(media: &PluginMedia) -> std::result::Result<(), PluginError> {
    let valid = match media {
        PluginMedia::ImageFrame {
            width,
            height,
            stride,
            data,
            ..
        } => {
            *width > 0
                && *height > 0
                && *stride >= width.saturating_mul(4)
                && u64::from(*stride).saturating_mul(u64::from(*height)) == data.len() as u64
        }
        PluginMedia::PcmAudio {
            sample_rate,
            channels,
            frame_count,
            data,
            ..
        } => {
            *sample_rate > 0
                && *channels > 0
                && frame_count
                    .saturating_mul(u64::from(*channels))
                    .saturating_mul(2)
                    == data.len() as u64
        }
        PluginMedia::EncodedPayload {
            format,
            data,
            regions,
        } => {
            !format.is_empty()
                && !data.is_empty()
                && validate_payload_regions(regions, data.len() as u64)
        }
    };
    valid.then_some(()).ok_or_else(|| PluginError {
        code: PluginErrorCode::InvalidInput,
        message: "plugin media geometry does not match its payload".to_owned(),
        retryable: false,
    })
}

fn validate_payload_regions(regions: &[PluginPayloadRegion], data_length: u64) -> bool {
    if regions.is_empty() {
        return false;
    }
    let mut previous_end = 0;
    for region in regions {
        let Some(end) = region.offset.checked_add(region.length) else {
            return false;
        };
        if region.length == 0
            || region.offset < previous_end
            || end > data_length
            || validate_identifier("payload region", &region.kind).is_err()
        {
            return false;
        }
        previous_end = end;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PluginManifest {
        PluginManifest {
            manifest_version: PLUGIN_MANIFEST_VERSION,
            abi_version: PLUGIN_ABI_VERSION,
            id: "example.noise".to_owned(),
            name: "Example Noise".to_owned(),
            version: "1.0.0".to_owned(),
            filters: vec![PluginFilterMetadata {
                id: "noise".to_owned(),
                name: "Noise".to_owned(),
                description: "Adds deterministic bounded noise".to_owned(),
                domains: vec![PluginDomain::ImageFrame, PluginDomain::PcmAudio],
                deterministic: true,
                parameters: vec![PluginParameterMetadata {
                    id: "amount".to_owned(),
                    name: "Amount".to_owned(),
                    description: "Noise amount".to_owned(),
                    kind: PluginParameterKind::Float {
                        minimum: 0.0,
                        maximum: 1.0,
                        default: 0.25,
                    },
                }],
            }],
        }
    }

    #[test]
    fn manifest_round_trips_and_rejects_unknown_fields() {
        let manifest = manifest();
        let encoded = serde_json::to_vec(&manifest).unwrap();
        let decoded: PluginManifest = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded, manifest);
        assert!(decoded.validate().is_ok());
        assert!(serde_json::from_str::<PluginManifest>(
            r#"{"manifest_version":1,"abi_version":1,"id":"example","name":"Example","version":"1","filters":[],"unknown":true}"#,
        )
        .is_err());
    }

    #[test]
    fn manifest_rejects_versions_duplicates_and_invalid_defaults() {
        let mut invalid = manifest();
        invalid.abi_version += 1;
        assert!(invalid.validate().unwrap_err().contains("unsupported ABI"));

        let mut invalid = manifest();
        invalid.filters.push(invalid.filters[0].clone());
        assert!(invalid.validate().unwrap_err().contains("duplicate filter"));

        let mut invalid = manifest();
        let duplicate_parameter = invalid.filters[0].parameters[0].clone();
        invalid.filters[0].parameters.push(duplicate_parameter);
        assert!(invalid
            .validate()
            .unwrap_err()
            .contains("repeats parameter"));

        let mut invalid = manifest();
        invalid.filters[0].parameters[0].kind = PluginParameterKind::Integer {
            minimum: 10,
            maximum: 20,
            default: 5,
        };
        assert!(invalid.validate().unwrap_err().contains("invalid bounds"));

        let mut invalid = manifest();
        invalid.filters[0].parameters[0].kind = PluginParameterKind::Choice {
            choices: vec!["low".to_owned(), "high".to_owned()],
            default: "missing".to_owned(),
        };
        assert!(invalid.validate().unwrap_err().contains("invalid bounds"));
    }

    #[test]
    fn invocation_carries_seed_and_validates_typed_media() {
        let invocation = PluginInvocation {
            abi_version: PLUGIN_ABI_VERSION,
            invocation_id: 7,
            filter_id: "noise".to_owned(),
            seed: 42,
            parameters: BTreeMap::from([("amount".to_owned(), PluginValue::Float(0.25))]),
            input: PluginMedia::ImageFrame {
                width: 2,
                height: 1,
                stride: 8,
                pixel_format: PluginPixelFormat::Rgba8,
                data: vec![0; 8],
            },
        };
        let command = PluginCommand::Invoke(invocation.clone());
        let encoded = serde_json::to_vec(&command).unwrap();

        assert_eq!(
            serde_json::from_slice::<PluginCommand>(&encoded).unwrap(),
            command
        );
        assert_eq!(invocation.seed, 42);
        assert!(invocation.validate().is_ok());

        let mut invalid = invocation;
        invalid.input = PluginMedia::ImageFrame {
            width: 2,
            height: 1,
            stride: 8,
            pixel_format: PluginPixelFormat::Rgba8,
            data: vec![0; 7],
        };
        assert_eq!(
            invalid.validate().unwrap_err().code,
            PluginErrorCode::InvalidInput
        );

        invalid.input = PluginMedia::EncodedPayload {
            format: "mp4".to_owned(),
            data: vec![0; 4],
            regions: vec![
                PluginPayloadRegion {
                    offset: 0,
                    length: 3,
                    kind: "header".to_owned(),
                    mutable: false,
                },
                PluginPayloadRegion {
                    offset: 2,
                    length: 2,
                    kind: "payload".to_owned(),
                    mutable: true,
                },
            ],
        };
        assert_eq!(
            invalid.validate().unwrap_err().code,
            PluginErrorCode::InvalidInput
        );
    }

    #[test]
    fn cancellation_progress_and_structured_errors_round_trip() {
        let cancel = PluginCommand::Cancel { invocation_id: 9 };
        assert_eq!(
            serde_json::from_slice::<PluginCommand>(&serde_json::to_vec(&cancel).unwrap()).unwrap(),
            cancel
        );

        let progress = PluginProgress {
            invocation_id: 9,
            completed: 2,
            total: 3,
            message: Some("filtering".to_owned()),
        };
        assert!(progress.validate().is_ok());
        let mut invalid_progress = progress.clone();
        invalid_progress.completed = 4;
        assert_eq!(
            invalid_progress.validate().unwrap_err().code,
            PluginErrorCode::InvalidInput
        );

        let event = PluginEvent::Failed {
            invocation_id: 9,
            error: PluginError {
                code: PluginErrorCode::ResourceLimit,
                message: "memory limit exceeded".to_owned(),
                retryable: false,
            },
        };
        assert_eq!(
            serde_json::from_slice::<PluginEvent>(&serde_json::to_vec(&event).unwrap()).unwrap(),
            event
        );
    }
}
