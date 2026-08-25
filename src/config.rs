use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

use crate::{
    DatabenderError, FilterDomain, FilterSpec, PluginRegistryConfig, Result, PIPELINE_PLAN_VERSION,
};

const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputPolicy {
    #[default]
    Replace,
    Protect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPreset {
    pub filters: Vec<FilterSpec>,
    pub filter_specifications: Vec<String>,
    pub seed: Option<u64>,
    pub output_policy: OutputPolicy,
    pub plugins: PluginRegistryConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    plan_version: Option<u32>,
    #[serde(default)]
    plugins: PluginRegistryConfig,
    #[serde(default)]
    presets: BTreeMap<String, Preset>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preset {
    filters: Vec<String>,
    seed: Option<u64>,
    #[serde(default)]
    output_policy: OutputPolicy,
    #[serde(default)]
    plugins: PluginRegistryConfig,
}

pub fn load_preset(path: impl AsRef<Path>, name: &str) -> Result<ResolvedPreset> {
    let path = path.as_ref();
    let config = load_config(path)?;
    let preset = config
        .presets
        .get(name)
        .ok_or_else(|| invalid(path, format!("preset {name:?} was not found")))?;
    if preset.filters.is_empty() {
        return Err(invalid(path, format!("preset {name:?} has no filters")));
    }
    let filters = preset
        .filters
        .iter()
        .map(|filter| FilterSpec::parse(filter))
        .collect::<Result<Vec<_>>>()?;
    let requires_plan_version = filters.iter().any(|filter| {
        matches!(
            filter.domain(),
            FilterDomain::JpegHuffmanTables
                | FilterDomain::EncodedPayload
                | FilterDomain::Mp3MainData
                | FilterDomain::OggPacket
                | FilterDomain::EncodedVideoPacket
        ) || filter.expert_graph().is_some()
    });
    if requires_plan_version {
        let declared = config.plan_version.ok_or_else(|| {
            invalid(
                path,
                format!(
                    "preset {name:?} uses encoded mutation or an expert graph and must declare plan_version = {PIPELINE_PLAN_VERSION}"
                ),
            )
        })?;
        if declared != PIPELINE_PLAN_VERSION {
            return Err(invalid(
                path,
                format!(
                    "unsupported pipeline plan version {declared}; expected {PIPELINE_PLAN_VERSION}"
                ),
            ));
        }
    }
    let mut plugins = config.plugins;
    plugins.merge(
        preset.plugins.directories.clone(),
        preset.plugins.disabled.clone(),
    );
    resolve_plugin_directories(path, &mut plugins);

    Ok(ResolvedPreset {
        filters,
        filter_specifications: preset.filters.clone(),
        seed: preset.seed,
        output_policy: preset.output_policy,
        plugins,
    })
}

pub fn load_plugin_config(path: impl AsRef<Path>) -> Result<PluginRegistryConfig> {
    let path = path.as_ref();
    let mut plugins = load_config(path)?.plugins;
    resolve_plugin_directories(path, &mut plugins);
    Ok(plugins)
}

fn load_config(path: &Path) -> Result<ConfigFile> {
    let encoded = fs::read_to_string(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let config: ConfigFile = toml::from_str(&encoded).map_err(|error| invalid(path, error))?;
    if config.version != CONFIG_VERSION {
        return Err(invalid(
            path,
            format!(
                "unsupported version {}; expected {CONFIG_VERSION}",
                config.version
            ),
        ));
    }
    Ok(config)
}

fn resolve_plugin_directories(path: &Path, plugins: &mut PluginRegistryConfig) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for directory in &mut plugins.directories {
        if directory.is_relative() {
            *directory = parent.join(&*directory);
        }
    }
    plugins.directories.sort();
    plugins.directories.dedup();
}

fn invalid(path: &Path, reason: impl ToString) -> DatabenderError {
    DatabenderError::InvalidConfiguration {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("databender.toml");
        fs::write(&path, contents).unwrap();
        (directory, path)
    }

    #[test]
    fn resolves_ordered_typed_preset() {
        let (_directory, path) = write_config(
            r#"
version = 1

[presets.shift]
filters = ["channel-shift:pixels=9", "invert"]
seed = 42
output_policy = "protect"
"#,
        );

        let preset = load_preset(path, "shift").unwrap();

        assert_eq!(preset.filters[0], FilterSpec::ChannelShift { pixels: 9 });
        assert_eq!(preset.filters[1], FilterSpec::Invert);
        assert_eq!(
            preset.filter_specifications,
            ["channel-shift:pixels=9", "invert"]
        );
        assert_eq!(preset.seed, Some(42));
        assert_eq!(preset.output_policy, OutputPolicy::Protect);
        assert_eq!(preset.plugins, PluginRegistryConfig::default());
    }

    #[test]
    fn merges_global_and_preset_plugin_configuration() {
        let (directory, path) = write_config(
            r#"
version = 1

[plugins]
directories = ["global"]
disabled = ["example.global"]

[presets.shift]
filters = ["invert"]

[presets.shift.plugins]
directories = ["preset"]
disabled = ["example.preset"]
"#,
        );

        let preset = load_preset(path, "shift").unwrap();

        assert_eq!(
            preset.plugins.directories,
            vec![
                directory.path().join("global"),
                directory.path().join("preset")
            ]
        );
        assert_eq!(
            preset.plugins.disabled,
            ["example.global".to_owned(), "example.preset".to_owned()]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn requires_matching_plan_version_for_advanced_presets() {
        let (_directory, missing) = write_config(
            r#"
version = 1

[presets.encoded]
filters = ["ogg-packet-noise"]
"#,
        );
        assert!(load_preset(missing, "encoded")
            .unwrap_err()
            .to_string()
            .contains("must declare plan_version = 1"));

        let (_directory, matching) = write_config(
            r#"
version = 1
plan_version = 1

[presets.encoded]
filters = ["ogg-packet-noise"]
"#,
        );
        assert!(load_preset(matching, "encoded").is_ok());
    }

    #[test]
    fn loads_plugin_configuration_without_presets() {
        let (directory, path) = write_config(
            r#"
version = 1
[plugins]
directories = ["plugins"]
disabled = ["example.off"]
"#,
        );

        let plugins = load_plugin_config(path).unwrap();

        assert_eq!(plugins.directories, vec![directory.path().join("plugins")]);
        assert!(plugins.disabled.contains("example.off"));
    }

    #[test]
    fn rejects_unknown_config_version() {
        let (_directory, path) = write_config(
            r#"
version = 2
[presets.shift]
filters = ["invert"]
"#,
        );

        assert!(load_preset(path, "shift")
            .unwrap_err()
            .to_string()
            .contains("unsupported version 2"));
    }
}
