use crate::{
    PluginCommand, PluginError, PluginErrorCode, PluginEvent, PluginInvocation, PluginMedia,
};

pub trait PluginFilter {
    fn invoke(
        &mut self,
        invocation: PluginInvocation,
    ) -> std::result::Result<PluginMedia, PluginError>;
}

pub fn dispatch(command_json: &[u8], filter: &mut impl PluginFilter) -> Vec<u8> {
    let event = match serde_json::from_slice::<PluginCommand>(command_json) {
        Ok(PluginCommand::Invoke(invocation)) => dispatch_invocation(invocation, filter),
        Ok(PluginCommand::Cancel { invocation_id }) => PluginEvent::Failed {
            invocation_id,
            error: PluginError {
                code: PluginErrorCode::Cancelled,
                message: "plugin invocation was cancelled".to_owned(),
                retryable: false,
            },
        },
        Err(error) => PluginEvent::Failed {
            invocation_id: 0,
            error: PluginError {
                code: PluginErrorCode::InvalidInput,
                message: format!("invalid plugin command JSON: {error}"),
                retryable: false,
            },
        },
    };
    serde_json::to_vec(&event).expect("plugin events are serializable")
}

pub fn pack_output(pointer: u32, length: u32) -> u64 {
    (u64::from(pointer) << 32) | u64::from(length)
}

fn dispatch_invocation(
    invocation: PluginInvocation,
    filter: &mut impl PluginFilter,
) -> PluginEvent {
    let invocation_id = invocation.invocation_id;
    if let Err(error) = invocation.validate() {
        return PluginEvent::Failed {
            invocation_id,
            error,
        };
    }
    match filter.invoke(invocation) {
        Ok(output) => PluginEvent::Completed {
            invocation_id,
            output,
        },
        Err(error) => PluginEvent::Failed {
            invocation_id,
            error,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::plugin::{PluginPayloadRegion, PluginValue, PLUGIN_ABI_VERSION};

    struct Increment;

    impl PluginFilter for Increment {
        fn invoke(
            &mut self,
            mut invocation: PluginInvocation,
        ) -> std::result::Result<PluginMedia, PluginError> {
            let PluginMedia::EncodedPayload { data, .. } = &mut invocation.input else {
                return Err(PluginError {
                    code: PluginErrorCode::UnsupportedDomain,
                    message: "expected encoded payload".to_owned(),
                    retryable: false,
                });
            };
            data[0] = data[0].wrapping_add(1);
            Ok(invocation.input)
        }
    }

    fn command() -> PluginCommand {
        PluginCommand::Invoke(PluginInvocation {
            abi_version: PLUGIN_ABI_VERSION,
            invocation_id: 7,
            filter_id: "example.increment".to_owned(),
            seed: 42,
            parameters: BTreeMap::from([("amount".to_owned(), PluginValue::Integer(1))]),
            input: PluginMedia::EncodedPayload {
                format: "fixture".to_owned(),
                data: vec![4],
                regions: vec![PluginPayloadRegion {
                    offset: 0,
                    length: 1,
                    kind: "payload".to_owned(),
                    mutable: true,
                }],
            },
        })
    }

    #[test]
    fn dispatches_validated_invocations_to_typed_filters() {
        let event: PluginEvent = serde_json::from_slice(&dispatch(
            &serde_json::to_vec(&command()).unwrap(),
            &mut Increment,
        ))
        .unwrap();

        let PluginEvent::Completed { output, .. } = event else {
            panic!("expected completion")
        };
        assert!(matches!(
            output,
            PluginMedia::EncodedPayload { data, .. } if data == vec![5]
        ));
    }

    #[test]
    fn turns_invalid_commands_and_cancellation_into_structured_failures() {
        let malformed: PluginEvent =
            serde_json::from_slice(&dispatch(b"not json", &mut Increment)).unwrap();
        let cancelled: PluginEvent = serde_json::from_slice(&dispatch(
            &serde_json::to_vec(&PluginCommand::Cancel { invocation_id: 9 }).unwrap(),
            &mut Increment,
        ))
        .unwrap();

        assert!(matches!(
            malformed,
            PluginEvent::Failed {
                invocation_id: 0,
                error: PluginError {
                    code: PluginErrorCode::InvalidInput,
                    ..
                }
            }
        ));
        assert!(matches!(
            cancelled,
            PluginEvent::Failed {
                invocation_id: 9,
                error: PluginError {
                    code: PluginErrorCode::Cancelled,
                    ..
                }
            }
        ));
        assert_eq!(pack_output(3, 5), (3_u64 << 32) | 5);
    }
}
