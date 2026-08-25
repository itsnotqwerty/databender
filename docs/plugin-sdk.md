# Plugin SDK and Packaging

Databender's Rust guest SDK is the public `plugin_sdk` module. It decodes strict ABI commands, validates invocations, dispatches typed media to a `PluginFilter`, converts handler errors into structured failure events, serializes responses, and packs output pointers for the WebAssembly ABI.

## Typed Handler

Implement `PluginFilter` with no host access. The handler receives an owned invocation and returns media with the same layout and topology:

```rust
use databender::{PluginError, PluginErrorCode, PluginFilter, PluginInvocation, PluginMedia};

struct Invert;

impl PluginFilter for Invert {
    fn invoke(
        &mut self,
        mut invocation: PluginInvocation,
    ) -> Result<PluginMedia, PluginError> {
        let PluginMedia::ImageFrame { data, .. } = &mut invocation.input else {
            return Err(PluginError {
                code: PluginErrorCode::UnsupportedDomain,
                message: "expected an image frame".to_owned(),
                retryable: false,
            });
        };
        for pixel in data.chunks_exact_mut(4) {
            pixel[0] = 255 - pixel[0];
            pixel[1] = 255 - pixel[1];
            pixel[2] = 255 - pixel[2];
        }
        Ok(invocation.input)
    }
}
```

Plugin errors are plain structured values with a stable `PluginErrorCode`, message, and retryability. The current v1 API intentionally keeps this policy explicit.

The guest's exported `databender_run` function passes the input memory slice to `dispatch_plugin_command`, retains the returned `Vec<u8>` in guest memory, and returns `pack_output(pointer, length)`. `databender_alloc` must retain an input allocation of the requested size. Both buffers must remain alive until the host has read them. Guest code must export linear memory as `memory` and must not import WASI or host functions.

## Bundle Layout

Production bundles contain two adjacent files with the same base name:

```text
my-filter.plugin.json
my-filter.wasm
```

Build Rust guests for `wasm32-unknown-unknown` as a `cdylib`, strip debug information for release packaging, and copy the binary beside its manifest. Keep plugin and filter IDs stable. Increment the plugin version whenever output behavior changes; deterministic results are scoped to the plugin version as well as input, parameters, and seed.

The bundles under `examples/plugins` are deliberately small textual WAT conformance fixtures stored with the discovered `.wasm` name. They demonstrate exact exports and typed events and are executed by `tests/plugin_conformance.rs`. Production packages should contain binary WebAssembly.

Before distribution, run:

```bash
cargo test --test plugin_conformance
cargo run -- list-plugins --plugin-dir path/to/bundle
```

Conforming bundles must pass manifest validation, duplicate-ID detection, module size limits, import denial, export signature checks, sandbox execution, event decoding, invocation-ID matching, and media topology validation. Add fixtures for every domain declared by a plugin.

## ABI Migration Policy

Manifest and ABI versions are independent unsigned integers. Databender v1 requires exact equality for both; it does not silently negotiate unknown versions.

- Backward-compatible documentation clarifications and host bug fixes do not change a version.
- New optional manifest fields require a new manifest version because v1 rejects unknown fields.
- Any command, event, media layout, export signature, or behavioral-contract change requires a new ABI version.
- A host may support multiple ABI versions concurrently in the future, but each invocation uses one exact version and one validator/runtime path.
- Removed versions require release-note notice, a migration guide, and at least one preceding deprecation cycle after multi-version support exists.

Package the manifest and module as an indivisible release artifact. Signatures and registries are outside ABI v1; distributors should publish checksums and preserve immutable versioned artifacts.

The host's capability boundary, dependency assumptions, denial-of-service limits, and plugin-authenticity non-goals are defined in the [Threat Model](threat-model.md). Version-1 migration steps are in [Migrating to v0.6](migration-v0.6.md).