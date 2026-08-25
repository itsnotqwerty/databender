# Plugin Manifest and ABI

Guest implementation, examples, conformance, packaging, and migration policy are documented in [Plugin SDK and Packaging](plugin-sdk.md).

Databender plugin manifests and host messages are versioned JSON contracts. The current manifest and ABI versions are both `1`; unsupported versions are rejected before invocation.

This contract defines data exchange only. Discovery, WebAssembly execution, capability grants, and resource limits are separate runtime responsibilities.

## Discovery and Compatibility

A plugin bundle consists of adjacent files with the same base name: `<name>.plugin.json` contains the manifest and `<name>.wasm` contains the module. `PluginRegistry::discover` scans explicitly configured directories in deterministic path order and rejects duplicate plugin IDs.

Discovery records compatible, incompatible, enabled, and disabled plugins rather than hiding unavailable bundles. Compatibility checks the manifest and ABI versions, module byte limit, WebAssembly decoding, absence of imports, exported memory, and the exact allocator and run signatures. It does not instantiate or execute the module. Provenance includes the plugin ID, version, manifest path, module path, and filter metadata.

Version-1 configuration supports global and preset-specific registry settings. Relative directories are resolved from the configuration file:

```toml
version = 1

[plugins]
directories = ["plugins"]
disabled = ["example.experimental"]

[presets.glitch]
filters = ["invert"]

[presets.glitch.plugins]
directories = ["project-plugins"]
disabled = ["example.noisy"]
```

Global settings are merged with preset settings, then explicit CLI directories and disabled IDs are added. `list-plugins`, `transform`, `batch`, and `tui` accept repeatable `--plugin-dir` and `--disable-plugin`; `--plugin-config` loads global settings without requiring a preset. `list-plugins` displays compatibility, enabled state, declared filters/domains, and manifest provenance.

Registry availability configures discovery and runtime clients. In the TUI, `[` and `]` select a discovered plugin and `t` toggles its enabled state. Plugin filters are not aliases for built-in `--filter` specifications; media-pipeline plugin stages will use the typed invocation API described below.

## Manifest

A manifest identifies the plugin and declares one or more filters:

```json
{
  "manifest_version": 1,
  "abi_version": 1,
  "id": "example.noise",
  "name": "Example Noise",
  "version": "1.0.0",
  "filters": [
    {
      "id": "noise",
      "name": "Noise",
      "description": "Adds deterministic bounded noise",
      "domains": ["image-frame", "pcm-audio"],
      "deterministic": true,
      "parameters": [
        {
          "id": "amount",
          "name": "Amount",
          "description": "Noise amount",
          "kind": {
            "type": "float",
            "minimum": 0.0,
            "maximum": 1.0,
            "default": 0.25
          }
        }
      ]
    }
  ]
}
```

Plugin, filter, and parameter IDs use lowercase ASCII letters, digits, dots, and hyphens. Filter and parameter IDs must be unique within their owning scope. Supported parameter kinds are Boolean, bounded integer, bounded finite float, and a nonempty choice set with a valid default.

The supported domains are:

- `image-frame`: RGBA8 pixels with explicit width, height, and stride.
- `pcm-audio`: signed 16-bit little-endian interleaved samples with explicit rate, channels, and frame count.
- `encoded-payload`: structure-aware bytes labeled with their media format and ordered regions containing offset, length, semantic kind, and mutability. Regions must be nonempty, non-overlapping, and within the payload.

`deterministic: true` promises identical output for the same plugin version, invocation seed, parameters, and input bytes.

## Host Commands

`PluginCommand` is a tagged `invoke` or `cancel` message. Every invocation carries the ABI version, invocation ID, fully qualified filter ID, deterministic seed, typed parameters, and one typed media payload. Cancellation identifies the active invocation without relying on shared process state.

Media geometry is validated before execution. Image payload length must equal `stride * height`; PCM payload length must equal `frame_count * channels * 2`; encoded payloads require a nonempty format and body.

## Plugin Events

`PluginEvent` is a tagged progress, completed, or failed message. Progress requires a positive total and `completed <= total`. Completion returns one typed media payload.

Failures carry a stable code, human-readable message, and retryability flag. Version 1 defines these codes:

- `abi-mismatch`
- `invalid-input`
- `invalid-parameter`
- `unsupported-domain`
- `cancelled`
- `resource-limit`
- `execution-failed`

Hosts must treat unknown manifest or ABI versions as incompatible. New optional behavior requires a new negotiated ABI version rather than silently changing payload interpretation.

## WebAssembly Runtime

`WasmPluginRuntime` executes import-free WebAssembly modules through Wasmtime. ABI version 1 modules export:

- `memory`: one linear memory used for command and event JSON.
- `databender_alloc(length: i32) -> i32`: reserves input bytes and returns their pointer.
- `databender_run(pointer: i32, length: i32) -> i64`: processes one `PluginCommand::Invoke` and returns an event pointer in the high 32 bits and length in the low 32 bits.

Modules with any import are rejected before instantiation. The runtime does not configure WASI, filesystem paths, environment variables, networking, clocks, random sources, process execution, or host callbacks. ABI version 1 grants a temporary-storage limit of zero.

Default sandbox limits are 4 MiB of module bytes, 64 MiB of linear memory, 32 MiB each for encoded input and output, one memory, one table, one instance, and 50 million fuel units. Limits are configurable through `PluginSandboxLimits`. Fuel exhaustion, failed memory growth, oversized modules or messages, and instantiation limit failures become `resource-limit` errors.

Cancellation increments the Wasmtime engine epoch and interrupts active guest code. The runtime validates the returned event, invocation ID, payload geometry, and media layout before accepting it. Image dimensions/stride/pixel format, PCM rate/channels/sample format/frame count, and encoded format/region topology cannot change during a filter invocation.