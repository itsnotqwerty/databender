#![no_main]

use databender::{PluginCommand, PluginEvent};
use databender::plugin::PluginManifest;
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
});
