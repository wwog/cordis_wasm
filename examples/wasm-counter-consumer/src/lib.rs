use cordis_guest::host::{CallContext, EventId, EventMode, EventReply, KernelError, ServiceId};
use cordis_guest::plugin::{Guest, PluginDescriptor};

const COUNTER_ABI: [u8; 32] = [0x43; 32];
const GET_METHOD: u32 = 1;

struct CounterConsumer;

impl Guest for CounterConsumer {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-counter-consumer".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: vec![counter_service()],
            provide: Vec::new(),
            config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
            capabilities: Vec::new(),
        }
    }

    fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
        let _: u64 = cordis_guest::call_service(&context, &counter_service(), GET_METHOD, &1_u64)?;
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        Ok(())
    }

    fn call_service(
        _context: CallContext,
        _service: ServiceId,
        _method: u32,
        _payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        Err(KernelError::UndeclaredDependency(
            "consumer provides no service".into(),
        ))
    }

    fn handle_event(
        _context: CallContext,
        _event: EventId,
        _listener_id: u64,
        _mode: EventMode,
        payload: Vec<u8>,
        _next_token: Option<u64>,
    ) -> Result<EventReply, KernelError> {
        Ok(EventReply::ContinueValue(payload))
    }
}

fn counter_service() -> ServiceId {
    ServiceId {
        name: "example.counter".into(),
        abi_hash: COUNTER_ABI.to_vec(),
    }
}

cordis_guest::export_plugin!(CounterConsumer);
