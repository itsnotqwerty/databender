use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    plugin::PluginManifest, DatabenderError, PluginSandboxLimits, Result, WasmPluginRuntime,
};

const MANIFEST_SUFFIX: &str = ".plugin.json";

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRegistryConfig {
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

impl PluginRegistryConfig {
    pub fn merge(
        &mut self,
        directories: impl IntoIterator<Item = PathBuf>,
        disabled: impl IntoIterator<Item = String>,
    ) {
        self.directories.extend(directories);
        self.directories.sort();
        self.directories.dedup();
        self.disabled.extend(disabled);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginCompatibility {
    Compatible,
    Incompatible(String),
}

impl PluginCompatibility {
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub manifest_path: PathBuf,
    pub module_path: PathBuf,
    pub enabled: bool,
    pub compatibility: PluginCompatibility,
}

impl DiscoveredPlugin {
    pub fn available(&self) -> bool {
        self.enabled && self.compatibility.is_compatible()
    }

    pub fn provenance(&self) -> String {
        format!(
            "{} {} ({})",
            self.manifest.id,
            self.manifest.version,
            self.manifest_path.display()
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginRegistry {
    plugins: Vec<DiscoveredPlugin>,
}

impl PluginRegistry {
    pub fn discover(config: &PluginRegistryConfig) -> Result<Self> {
        let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default())
            .map_err(|error| invalid_registry(error.message))?;
        let mut manifests = Vec::new();
        for directory in &config.directories {
            let entries = fs::read_dir(directory).map_err(|source| DatabenderError::Io {
                path: directory.clone(),
                source,
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| DatabenderError::Io {
                    path: directory.clone(),
                    source,
                })?;
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(MANIFEST_SUFFIX))
                {
                    manifests.push(path);
                }
            }
        }
        manifests.sort();
        manifests.dedup();

        let mut ids = HashSet::new();
        let mut plugins = Vec::with_capacity(manifests.len());
        for manifest_path in manifests {
            let encoded = fs::read(&manifest_path).map_err(|source| DatabenderError::Io {
                path: manifest_path.clone(),
                source,
            })?;
            let manifest: PluginManifest = serde_json::from_slice(&encoded).map_err(|error| {
                invalid_registry(format!(
                    "could not parse plugin manifest {}: {error}",
                    manifest_path.display()
                ))
            })?;
            manifest.validate_structure().map_err(|reason| {
                invalid_registry(format!(
                    "invalid plugin manifest {}: {reason}",
                    manifest_path.display()
                ))
            })?;
            if !ids.insert(manifest.id.clone()) {
                return Err(invalid_registry(format!(
                    "duplicate plugin id {:?}",
                    manifest.id
                )));
            }
            let module_path = module_path(&manifest_path)?;
            let compatibility = manifest
                .compatibility_error()
                .err()
                .or_else(|| match fs::read(&module_path) {
                    Ok(wasm) => runtime
                        .validate_module(&wasm)
                        .err()
                        .map(|error| error.message),
                    Err(error) => Some(format!(
                        "could not read module {}: {error}",
                        module_path.display()
                    )),
                })
                .map_or(
                    PluginCompatibility::Compatible,
                    PluginCompatibility::Incompatible,
                );
            plugins.push(DiscoveredPlugin {
                enabled: !config.disabled.contains(&manifest.id),
                manifest,
                manifest_path,
                module_path,
                compatibility,
            });
        }
        Ok(Self { plugins })
    }

    pub fn plugins(&self) -> &[DiscoveredPlugin] {
        &self.plugins
    }

    pub fn toggle(&mut self, index: usize) -> Result<bool> {
        let plugin = self
            .plugins
            .get_mut(index)
            .ok_or_else(|| invalid_registry(format!("plugin index {index} is out of range")))?;
        plugin.enabled = !plugin.enabled;
        Ok(plugin.enabled)
    }
}

fn module_path(manifest_path: &Path) -> Result<PathBuf> {
    let name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(MANIFEST_SUFFIX))
        .ok_or_else(|| invalid_registry("plugin manifest name is invalid"))?;
    Ok(manifest_path.with_file_name(format!("{name}.wasm")))
}

fn invalid_registry(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidConfiguration {
        path: PathBuf::from("<plugin-registry>"),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        PluginDomain, PluginFilterMetadata, PLUGIN_ABI_VERSION, PLUGIN_MANIFEST_VERSION,
    };

    fn write_bundle(directory: &Path, name: &str, id: &str, abi_version: u32, module: bool) {
        let manifest = PluginManifest {
            manifest_version: PLUGIN_MANIFEST_VERSION,
            abi_version,
            id: id.to_owned(),
            name: id.to_owned(),
            version: "1.0.0".to_owned(),
            filters: vec![PluginFilterMetadata {
                id: "invert".to_owned(),
                name: "Invert".to_owned(),
                description: "Inverts a frame".to_owned(),
                domains: vec![PluginDomain::ImageFrame],
                deterministic: true,
                parameters: Vec::new(),
            }],
        };
        fs::write(
            directory.join(format!("{name}{MANIFEST_SUFFIX}")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        if module {
            fs::write(
                directory.join(format!("{name}.wasm")),
                br#"(module
                    (memory (export "memory") 1)
                    (func (export "databender_alloc") (param i32) (result i32) i32.const 0)
                    (func (export "databender_run") (param i32 i32) (result i64) i64.const 0))"#,
            )
            .unwrap();
        }
    }

    #[test]
    fn discovers_compatible_disabled_and_incompatible_plugins() {
        let directory = tempfile::tempdir().unwrap();
        write_bundle(directory.path(), "a", "example.a", PLUGIN_ABI_VERSION, true);
        write_bundle(
            directory.path(),
            "b",
            "example.b",
            PLUGIN_ABI_VERSION,
            false,
        );
        write_bundle(
            directory.path(),
            "c",
            "example.c",
            PLUGIN_ABI_VERSION + 1,
            true,
        );
        let config = PluginRegistryConfig {
            directories: vec![directory.path().to_path_buf()],
            disabled: BTreeSet::from(["example.a".to_owned()]),
        };

        let registry = PluginRegistry::discover(&config).unwrap();

        assert_eq!(registry.plugins.len(), 3);
        assert!(!registry.plugins[0].enabled);
        assert!(registry.plugins[0].compatibility.is_compatible());
        assert!(registry.plugins[1].enabled);
        assert!(!registry.plugins[1].compatibility.is_compatible());
        assert!(!registry.plugins[2].compatibility.is_compatible());
        assert!(registry.plugins[0].provenance().contains("a.plugin.json"));
    }

    #[test]
    fn rejects_duplicate_plugin_ids() {
        let directory = tempfile::tempdir().unwrap();
        write_bundle(
            directory.path(),
            "a",
            "example.same",
            PLUGIN_ABI_VERSION,
            true,
        );
        write_bundle(
            directory.path(),
            "b",
            "example.same",
            PLUGIN_ABI_VERSION,
            true,
        );
        let config = PluginRegistryConfig {
            directories: vec![directory.path().to_path_buf()],
            disabled: BTreeSet::new(),
        };

        assert!(PluginRegistry::discover(&config)
            .unwrap_err()
            .to_string()
            .contains("duplicate plugin id"));
    }
}
