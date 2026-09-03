use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use std::cell::RefCell;

const COUNTER_ABI: [u8; 32] = [0x43; 32];
const GET_METHOD: u32 = 1;

thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static VALUE: RefCell<u64> = const { RefCell::new(0) };
}

struct CounterProvider;

impl Guest for CounterProvider {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-counter-provider".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: Vec::new(),
            provide: vec![counter_service()],
            config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
            capabilities: Vec::new(),
        }
    }

    fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
        let registration = host::provide_service(context, &counter_service())?;
        REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        REGISTRATION.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext,
        service: ServiceId,
        method: u32,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        if service.name != counter_service().name || method != GET_METHOD {
            return Err(KernelError::InvalidArgument(
                "unknown service method".into(),
            ));
        }
        let increment = if payload.is_empty() {
            1
        } else {
            cordis_guest::decode::<u64>(&payload)?
        };
        let value = VALUE.with(|value| {
            let mut value = value.borrow_mut();
            *value += increment;
            *value
        });
        cordis_guest::encode(&value)
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

cordis_guest::export_plugin!(CounterProvider);
