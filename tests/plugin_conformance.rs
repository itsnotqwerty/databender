use std::{collections::BTreeMap, fs, path::PathBuf};

use databender::{
    plugin::{PluginPayloadRegion, PluginPixelFormat, PluginValue, PLUGIN_ABI_VERSION},
    CancellationToken, PluginEvent, PluginInvocation, PluginMedia, PluginRegistry,
    PluginRegistryConfig, PluginSandboxLimits, WasmPluginRuntime,
};

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins")
}

fn invocation(filter_id: &str, input: PluginMedia) -> PluginInvocation {
    PluginInvocation {
        abi_version: PLUGIN_ABI_VERSION,
        invocation_id: 1,
        filter_id: filter_id.to_owned(),
        seed: 42,
        parameters: BTreeMap::<String, PluginValue>::new(),
        input,
    }
}

#[test]
fn packaged_examples_conform_to_discovery_and_runtime_contracts() {
    let registry = PluginRegistry::discover(&PluginRegistryConfig {
        directories: vec![examples()],
        disabled: Default::default(),
    })
    .unwrap();
    assert_eq!(registry.plugins().len(), 2);
    assert!(registry.plugins().iter().all(|plugin| plugin.available()));
    let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default()).unwrap();

    let cases = [
        (
            "org.databender.example.image-invert",
            invocation(
                "invert",
                PluginMedia::ImageFrame {
                    width: 1,
                    height: 1,
                    stride: 4,
                    pixel_format: PluginPixelFormat::Rgba8,
                    data: vec![10, 20, 30, 255],
                },
            ),
            PluginMedia::ImageFrame {
                width: 1,
                height: 1,
                stride: 4,
                pixel_format: PluginPixelFormat::Rgba8,
                data: vec![245, 235, 225, 255],
            },
        ),
        (
            "org.databender.example.payload-increment",
            invocation(
                "increment",
                PluginMedia::EncodedPayload {
                    format: "fixture".to_owned(),
                    data: vec![4],
                    regions: vec![PluginPayloadRegion {
                        offset: 0,
                        length: 1,
                        kind: "payload".to_owned(),
                        mutable: true,
                    }],
                },
            ),
            PluginMedia::EncodedPayload {
                format: "fixture".to_owned(),
                data: vec![5],
                regions: vec![PluginPayloadRegion {
                    offset: 0,
                    length: 1,
                    kind: "payload".to_owned(),
                    mutable: true,
                }],
            },
        ),
    ];

    for (plugin_id, invocation, expected) in cases {
        let plugin = registry
            .plugins()
            .iter()
            .find(|plugin| plugin.manifest.id == plugin_id)
            .unwrap();
        let wasm = fs::read(&plugin.module_path).unwrap();
        let event = runtime
            .execute(&wasm, &invocation, &CancellationToken::default())
            .unwrap();
        assert_eq!(
            event,
            PluginEvent::Completed {
                invocation_id: 1,
                output: expected,
            }
        );
    }
}
