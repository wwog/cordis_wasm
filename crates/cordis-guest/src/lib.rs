//! Guest SDK for Cordis WebAssembly components.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Generated Cordis kernel imports and plugin exports.
#[allow(clippy::same_length_and_capacity)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "cordis-plugin",
        pub_export_macro: true,
    });
}

pub use bindings::cordis::kernel::host;
pub use bindings::exports::cordis::kernel::plugin;

/// Kernel ABI implemented by this SDK release.
pub const KERNEL_ABI: &str = "0.1";

/// Encodes a typed service/event value for the dynamic kernel boundary.
///
/// # Errors
///
/// Returns `invalid-argument` when serialization fails.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, host::KernelError> {
    rmp_serde::to_vec_named(value)
        .map_err(|error| host::KernelError::InvalidArgument(error.to_string()))
}

/// Decodes a typed service/event value received from the dynamic kernel boundary.
///
/// # Errors
///
/// Returns `invalid-argument` when the payload does not match `T`.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, host::KernelError> {
    rmp_serde::from_slice(bytes)
        .map_err(|error| host::KernelError::InvalidArgument(error.to_string()))
}

/// Encodes a plugin configuration schema for its descriptor.
///
/// # Errors
///
/// Returns `invalid-argument` if JSON serialization fails.
pub fn schema_json(value: &serde_json::Value) -> Result<Vec<u8>, host::KernelError> {
    serde_json::to_vec(value).map_err(|error| host::KernelError::InvalidArgument(error.to_string()))
}

/// Calls a host service with typed `MessagePack` input and output.
///
/// # Errors
///
/// Returns the host error or a request/reply codec error.
pub fn call_service<Req, Res>(
    context: &host::CallContext,
    service: &host::ServiceId,
    method: u32,
    request: &Req,
) -> Result<Res, host::KernelError>
where
    Req: Serialize,
    Res: DeserializeOwned,
{
    let payload = encode(request)?;
    let reply = host::call_service(*context, service, method, &payload)?;
    decode(&reply)
}

/// Exports a type implementing the generated plugin `Guest` trait.
#[macro_export]
macro_rules! export_plugin {
    ($component:ident) => {
        $crate::bindings::export!($component with_types_in $crate::bindings);
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Message {
        value: u64,
    }

    #[test]
    fn message_pack_round_trip() {
        let value = Message { value: 42 };
        assert_eq!(decode::<Message>(&encode(&value).unwrap()).unwrap(), value);
    }
}
