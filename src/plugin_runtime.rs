use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use wasmtime::{
    Config, Engine, ExternType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, ValType,
};

use crate::{
    CancellationToken, PluginCommand, PluginError, PluginErrorCode, PluginEvent, PluginInvocation,
    PluginMedia,
};

const ALLOC_EXPORT: &str = "databender_alloc";
const RUN_EXPORT: &str = "databender_run";
const MEMORY_EXPORT: &str = "memory";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginSandboxLimits {
    pub module_bytes: usize,
    pub memory_bytes: usize,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub fuel: u64,
    pub temporary_storage_bytes: usize,
}

impl Default for PluginSandboxLimits {
    fn default() -> Self {
        Self {
            module_bytes: 4 * 1024 * 1024,
            memory_bytes: 64 * 1024 * 1024,
            input_bytes: 32 * 1024 * 1024,
            output_bytes: 32 * 1024 * 1024,
            fuel: 50_000_000,
            temporary_storage_bytes: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WasmPluginRuntime {
    engine: Engine,
    limits: PluginSandboxLimits,
}

struct SandboxState {
    limits: StoreLimits,
}

impl WasmPluginRuntime {
    pub fn new(limits: PluginSandboxLimits) -> std::result::Result<Self, PluginError> {
        validate_limits(limits)?;
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(execution_error)?;
        Ok(Self { engine, limits })
    }

    pub fn execute(
        &self,
        wasm: &[u8],
        invocation: &PluginInvocation,
        cancellation: &CancellationToken,
    ) -> std::result::Result<PluginEvent, PluginError> {
        invocation.validate()?;
        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        let module = self.compile_and_validate(wasm)?;

        let command = serde_json::to_vec(&PluginCommand::Invoke(invocation.clone()))
            .map_err(execution_error)?;
        if command.len() > self.limits.input_bytes {
            return Err(resource_error(
                "plugin invocation exceeds the configured input limit",
            ));
        }

        let module = module;
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .instances(1)
            .tables(1)
            .memories(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            SandboxState {
                limits: store_limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_fuel(self.limits.fuel).map_err(execution_error)?;
        store.set_epoch_deadline(1);

        let linker = Linker::new(&self.engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|error| resource_error(error.to_string()))?;
        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| abi_error("plugin must export linear memory as `memory`"))?;
        let allocate = instance
            .get_typed_func::<i32, i32>(&mut store, ALLOC_EXPORT)
            .map_err(|error| abi_error(error.to_string()))?;
        let run = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, RUN_EXPORT)
            .map_err(|error| abi_error(error.to_string()))?;
        let input_length = i32::try_from(command.len())
            .map_err(|_| resource_error("plugin invocation is too large for the ABI"))?;
        let input_pointer = allocate
            .call(&mut store, input_length)
            .map_err(|error| trap_error(error, cancellation))?;
        let input_offset = usize::try_from(input_pointer)
            .map_err(|_| abi_error("plugin allocator returned a negative pointer"))?;
        memory
            .write(&mut store, input_offset, &command)
            .map_err(|error| abi_error(error.to_string()))?;

        let done = Arc::new(AtomicBool::new(false));
        let watcher =
            cancellation_watcher(self.engine.clone(), cancellation.clone(), Arc::clone(&done));
        let packed_output = run.call(&mut store, (input_pointer, input_length));
        done.store(true, Ordering::Release);
        let _ = watcher.join();
        let packed_output = packed_output.map_err(|error| trap_error(error, cancellation))? as u64;
        if cancellation.is_cancelled() {
            return Err(cancelled_error());
        }

        let output_pointer = (packed_output >> 32) as u32 as usize;
        let output_length = (packed_output as u32) as usize;
        if output_length > self.limits.output_bytes {
            return Err(resource_error(
                "plugin response exceeds the configured output limit",
            ));
        }
        let mut encoded_event = vec![0; output_length];
        memory
            .read(&store, output_pointer, &mut encoded_event)
            .map_err(|error| abi_error(error.to_string()))?;
        let event: PluginEvent = serde_json::from_slice(&encoded_event)
            .map_err(|error| abi_error(format!("invalid plugin event JSON: {error}")))?;
        validate_event(&event, invocation)?;
        Ok(event)
    }

    pub fn validate_module(&self, wasm: &[u8]) -> std::result::Result<(), PluginError> {
        self.compile_and_validate(wasm).map(|_| ())
    }

    fn compile_and_validate(&self, wasm: &[u8]) -> std::result::Result<Module, PluginError> {
        if wasm.len() > self.limits.module_bytes {
            return Err(resource_error(
                "plugin module exceeds the configured byte limit",
            ));
        }
        let module = Module::new(&self.engine, wasm).map_err(execution_error)?;
        if let Some(import) = module.imports().next() {
            return Err(PluginError {
                code: PluginErrorCode::UnsupportedDomain,
                message: format!(
                    "plugin imports are not permitted: {}::{}",
                    import.module(),
                    import.name()
                ),
                retryable: false,
            });
        }
        validate_export(&module, MEMORY_EXPORT, &[], &[], true)?;
        validate_export(
            &module,
            ALLOC_EXPORT,
            &[ValType::I32],
            &[ValType::I32],
            false,
        )?;
        validate_export(
            &module,
            RUN_EXPORT,
            &[ValType::I32, ValType::I32],
            &[ValType::I64],
            false,
        )?;
        Ok(module)
    }
}

fn validate_export(
    module: &Module,
    name: &str,
    parameters: &[ValType],
    results: &[ValType],
    memory: bool,
) -> std::result::Result<(), PluginError> {
    let export = module
        .get_export(name)
        .ok_or_else(|| abi_error(format!("plugin must export `{name}`")))?;
    if memory {
        return matches!(export, ExternType::Memory(_))
            .then_some(())
            .ok_or_else(|| abi_error(format!("plugin export `{name}` has the wrong type")));
    }
    let ExternType::Func(function) = export else {
        return Err(abi_error(format!(
            "plugin export `{name}` has the wrong type"
        )));
    };
    if value_types_match(function.params(), parameters)
        && value_types_match(function.results(), results)
    {
        Ok(())
    } else {
        Err(abi_error(format!(
            "plugin export `{name}` has the wrong signature"
        )))
    }
}

fn value_types_match(actual: impl Iterator<Item = ValType>, expected: &[ValType]) -> bool {
    let actual = actual.collect::<Vec<_>>();
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            std::mem::discriminant(actual) == std::mem::discriminant(expected)
        })
}

fn validate_limits(limits: PluginSandboxLimits) -> std::result::Result<(), PluginError> {
    if limits.module_bytes == 0
        || limits.memory_bytes < 64 * 1024
        || limits.input_bytes == 0
        || limits.output_bytes == 0
        || limits.fuel == 0
    {
        return Err(PluginError {
            code: PluginErrorCode::InvalidParameter,
            message: "sandbox limits must be positive and memory must allow one WebAssembly page"
                .to_owned(),
            retryable: false,
        });
    }
    if limits.temporary_storage_bytes != 0 {
        return Err(PluginError {
            code: PluginErrorCode::UnsupportedDomain,
            message: "plugin ABI v1 grants no temporary-storage capability".to_owned(),
            retryable: false,
        });
    }
    Ok(())
}

fn cancellation_watcher(
    engine: Engine,
    cancellation: CancellationToken,
    done: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !done.load(Ordering::Acquire) {
            if cancellation.is_cancelled() {
                engine.increment_epoch();
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    })
}

fn validate_event(
    event: &PluginEvent,
    invocation: &PluginInvocation,
) -> std::result::Result<(), PluginError> {
    let invocation_id = match event {
        PluginEvent::Progress(progress) => {
            progress.validate()?;
            progress.invocation_id
        }
        PluginEvent::Completed {
            invocation_id,
            output,
        } => {
            let output_invocation = PluginInvocation {
                input: output.clone(),
                ..invocation.clone()
            };
            output_invocation.validate()?;
            if !same_layout(&invocation.input, output) {
                return Err(abi_error("plugin output changed the media layout"));
            }
            *invocation_id
        }
        PluginEvent::Failed { invocation_id, .. } => *invocation_id,
    };
    if invocation_id != invocation.invocation_id {
        return Err(abi_error(
            "plugin event invocation ID does not match the request",
        ));
    }
    Ok(())
}

fn same_layout(input: &PluginMedia, output: &PluginMedia) -> bool {
    match (input, output) {
        (
            PluginMedia::ImageFrame {
                width: input_width,
                height: input_height,
                stride: input_stride,
                pixel_format: input_format,
                ..
            },
            PluginMedia::ImageFrame {
                width: output_width,
                height: output_height,
                stride: output_stride,
                pixel_format: output_format,
                ..
            },
        ) => {
            (input_width, input_height, input_stride, input_format)
                == (output_width, output_height, output_stride, output_format)
        }
        (
            PluginMedia::PcmAudio {
                sample_rate: input_rate,
                channels: input_channels,
                sample_format: input_format,
                frame_count: input_frames,
                ..
            },
            PluginMedia::PcmAudio {
                sample_rate: output_rate,
                channels: output_channels,
                sample_format: output_format,
                frame_count: output_frames,
                ..
            },
        ) => {
            (input_rate, input_channels, input_format, input_frames)
                == (output_rate, output_channels, output_format, output_frames)
        }
        (
            PluginMedia::EncodedPayload {
                format: input_format,
                regions: input_regions,
                ..
            },
            PluginMedia::EncodedPayload {
                format: output_format,
                regions: output_regions,
                ..
            },
        ) => input_format == output_format && input_regions == output_regions,
        _ => false,
    }
}

fn trap_error(error: wasmtime::Error, cancellation: &CancellationToken) -> PluginError {
    if cancellation.is_cancelled() {
        cancelled_error()
    } else {
        resource_error(error.to_string())
    }
}

fn abi_error(message: impl Into<String>) -> PluginError {
    PluginError {
        code: PluginErrorCode::InvalidInput,
        message: message.into(),
        retryable: false,
    }
}

fn execution_error(error: impl std::fmt::Display) -> PluginError {
    PluginError {
        code: PluginErrorCode::ExecutionFailed,
        message: error.to_string(),
        retryable: false,
    }
}

fn resource_error(message: impl Into<String>) -> PluginError {
    PluginError {
        code: PluginErrorCode::ResourceLimit,
        message: message.into(),
        retryable: false,
    }
}

fn cancelled_error() -> PluginError {
    PluginError {
        code: PluginErrorCode::Cancelled,
        message: "plugin invocation was cancelled".to_owned(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::plugin::{PluginPayloadRegion, PluginValue, PLUGIN_ABI_VERSION};

    fn invocation() -> PluginInvocation {
        invocation_with(PluginMedia::EncodedPayload {
            format: "fixture".to_owned(),
            data: vec![0],
            regions: vec![PluginPayloadRegion {
                offset: 0,
                length: 1,
                kind: "payload".to_owned(),
                mutable: true,
            }],
        })
    }

    fn invocation_with(input: PluginMedia) -> PluginInvocation {
        PluginInvocation {
            abi_version: PLUGIN_ABI_VERSION,
            invocation_id: 7,
            filter_id: "example.noise".to_owned(),
            seed: 42,
            parameters: BTreeMap::from([("amount".to_owned(), PluginValue::Integer(1))]),
            input,
        }
    }

    fn completed_event() -> PluginEvent {
        PluginEvent::Completed {
            invocation_id: 7,
            output: PluginMedia::EncodedPayload {
                format: "fixture".to_owned(),
                data: vec![1],
                regions: vec![PluginPayloadRegion {
                    offset: 0,
                    length: 1,
                    kind: "payload".to_owned(),
                    mutable: true,
                }],
            },
        }
    }

    fn static_event_module(event: &PluginEvent) -> Vec<u8> {
        let encoded = serde_json::to_vec(event).unwrap();
        let escaped = encoded
            .iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>();
        format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 0) "{escaped}")
                (func (export "databender_alloc") (param i32) (result i32)
                    i32.const 4096)
                (func (export "databender_run") (param i32 i32) (result i64)
                    i64.const {})
            )"#,
            encoded.len()
        )
        .into_bytes()
    }

    fn spinning_module() -> &'static [u8] {
        br#"(module
            (memory (export "memory") 1)
            (func (export "databender_alloc") (param i32) (result i32)
                i32.const 4096)
            (func (export "databender_run") (param i32 i32) (result i64)
                (loop $spin (br $spin))
                i64.const 0)
        )"#
    }

    #[test]
    fn executes_import_free_plugin_with_typed_output() {
        let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default()).unwrap();

        let event = runtime
            .execute(
                &static_event_module(&completed_event()),
                &invocation(),
                &CancellationToken::default(),
            )
            .unwrap();

        assert_eq!(event, completed_event());
    }

    #[test]
    fn executes_image_frame_and_pcm_audio_interfaces() {
        let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default()).unwrap();
        let cases = [
            (
                PluginMedia::ImageFrame {
                    width: 1,
                    height: 1,
                    stride: 4,
                    pixel_format: crate::plugin::PluginPixelFormat::Rgba8,
                    data: vec![0, 0, 0, 255],
                },
                PluginMedia::ImageFrame {
                    width: 1,
                    height: 1,
                    stride: 4,
                    pixel_format: crate::plugin::PluginPixelFormat::Rgba8,
                    data: vec![255, 255, 255, 255],
                },
            ),
            (
                PluginMedia::PcmAudio {
                    sample_rate: 48_000,
                    channels: 1,
                    sample_format: crate::plugin::PluginSampleFormat::Signed16LittleEndian,
                    frame_count: 1,
                    data: vec![0, 0],
                },
                PluginMedia::PcmAudio {
                    sample_rate: 48_000,
                    channels: 1,
                    sample_format: crate::plugin::PluginSampleFormat::Signed16LittleEndian,
                    frame_count: 1,
                    data: vec![1, 0],
                },
            ),
        ];

        for (input, output) in cases {
            let event = PluginEvent::Completed {
                invocation_id: 7,
                output: output.clone(),
            };
            assert_eq!(
                runtime
                    .execute(
                        &static_event_module(&event),
                        &invocation_with(input),
                        &CancellationToken::default(),
                    )
                    .unwrap(),
                event
            );
        }
    }

    #[test]
    fn rejects_all_host_imports_and_temporary_storage() {
        let runtime = WasmPluginRuntime::new(PluginSandboxLimits::default()).unwrap();
        let imported = br#"(module
            (import "wasi_snapshot_preview1" "fd_write" (func))
            (memory (export "memory") 1)
            (func (export "databender_alloc") (param i32) (result i32) i32.const 0)
            (func (export "databender_run") (param i32 i32) (result i64) i64.const 0)
        )"#;

        let error = runtime
            .execute(imported, &invocation(), &CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, PluginErrorCode::UnsupportedDomain);

        let limits = PluginSandboxLimits {
            temporary_storage_bytes: 1,
            ..PluginSandboxLimits::default()
        };
        assert_eq!(
            WasmPluginRuntime::new(limits).unwrap_err().code,
            PluginErrorCode::UnsupportedDomain
        );
    }

    #[test]
    fn enforces_memory_output_and_fuel_limits() {
        let memory_limits = PluginSandboxLimits {
            memory_bytes: 64 * 1024,
            ..PluginSandboxLimits::default()
        };
        let runtime = WasmPluginRuntime::new(memory_limits).unwrap();
        let oversized_memory = br#"(module
            (memory (export "memory") 2)
            (func (export "databender_alloc") (param i32) (result i32) i32.const 0)
            (func (export "databender_run") (param i32 i32) (result i64) i64.const 0)
        )"#;
        assert_eq!(
            runtime
                .execute(
                    oversized_memory,
                    &invocation(),
                    &CancellationToken::default()
                )
                .unwrap_err()
                .code,
            PluginErrorCode::ResourceLimit
        );

        let event_module = static_event_module(&completed_event());
        let output_limits = PluginSandboxLimits {
            output_bytes: 1,
            ..PluginSandboxLimits::default()
        };
        assert_eq!(
            WasmPluginRuntime::new(output_limits)
                .unwrap()
                .execute(&event_module, &invocation(), &CancellationToken::default())
                .unwrap_err()
                .code,
            PluginErrorCode::ResourceLimit
        );

        let fuel_limits = PluginSandboxLimits {
            fuel: 1_000,
            ..PluginSandboxLimits::default()
        };
        assert_eq!(
            WasmPluginRuntime::new(fuel_limits)
                .unwrap()
                .execute(
                    spinning_module(),
                    &invocation(),
                    &CancellationToken::default()
                )
                .unwrap_err()
                .code,
            PluginErrorCode::ResourceLimit
        );
    }

    #[test]
    fn cancellation_interrupts_running_plugin() {
        let runtime = WasmPluginRuntime::new(PluginSandboxLimits {
            fuel: u64::MAX,
            ..PluginSandboxLimits::default()
        })
        .unwrap();
        let cancellation = CancellationToken::default();
        let cancel_from_thread = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancel_from_thread.cancel();
        });

        let error = runtime
            .execute(spinning_module(), &invocation(), &cancellation)
            .unwrap_err();
        canceller.join().unwrap();

        assert_eq!(error.code, PluginErrorCode::Cancelled);
    }
}
