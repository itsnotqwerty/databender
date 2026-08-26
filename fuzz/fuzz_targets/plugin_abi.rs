#![no_main]

use std::sync::OnceLock;

use databender::{PluginCommand, PluginEvent};
use databender::plugin::PluginManifest;
use databender::{PluginSandboxLimits, WasmPluginRuntime};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    if let Ok(manifest) = serde_json::from_slice::<PluginManifest>(input) {
        let _ = manifest.validate();
    }
    if let Ok(command) = serde_json::from_slice::<PluginCommand>(input) {
        if let PluginCommand::Invoke(invocation) = command {
            let _ = invocation.validate();
        }
    }
    if let Ok(event) = serde_json::from_slice::<PluginEvent>(input) {
        if let PluginEvent::Progress(progress) = event {
            let _ = progress.validate();
        }
    }
    let runtime = RUNTIME.get_or_init(|| {
        WasmPluginRuntime::new(PluginSandboxLimits::default()).expect("valid fuzz runtime limits")
    });
    let _ = runtime.validate_module(input);
});

static RUNTIME: OnceLock<WasmPluginRuntime> = OnceLock::new();
