//! Dynamic WebAssembly plugin: observes HTTP request lifecycles emitted by the
//! native HTTP gateway and logs structured fields for each phase.

use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

// Shared wire protocol with the native gateway — see `http_gateway.rs`.
const REQUEST_STARTED_ABI: [u8; 32] = [0xA1; 32];
const REQUEST_FINISHED_ABI: [u8; 32] = [0xB2; 32];
const STARTED_LISTENER: u64 = 1;
const FINISHED_LISTENER: u64 = 2;

thread_local! {
    static STARTED: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static FINISHED: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
}

#[derive(Deserialize, Serialize)]
struct RequestStarted {
    path: String,
    method: String,
    headers: Vec<(String, String)>,
}

#[derive(Deserialize, Serialize)]
struct RequestFinished {
    status: u16,
    bytes: usize,
    duration_ms: u64,
}

fn started_event() -> EventId {
    EventId {
        name: "http.request.started".into(),
        abi_hash: REQUEST_STARTED_ABI.to_vec(),
    }
}

fn finished_event() -> EventId {
    EventId {
        name: "http.request.finished".into(),
        abi_hash: REQUEST_FINISHED_ABI.to_vec(),
    }
}

struct LogPlugin;

impl Guest for LogPlugin {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-log-plugin".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: Vec::new(),
            provide: Vec::new(),
            config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
            capabilities: Vec::new(),
        }
    }

    fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
        let started =
            host::register_listener(context, &started_event(), STARTED_LISTENER, EventMode::Serial)?;
        let finished =
            host::register_listener(context, &finished_event(), FINISHED_LISTENER, EventMode::Serial)?;
        STARTED.with(|slot| *slot.borrow_mut() = Some(started));
        FINISHED.with(|slot| *slot.borrow_mut() = Some(finished));
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        STARTED.with(|slot| slot.borrow_mut().take());
        FINISHED.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext,
        _service: ServiceId,
        _method: u32,
        _payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        Err(KernelError::UndeclaredDependency(
            "log plugin provides no service".into(),
        ))
    }

    fn handle_event(
        context: CallContext,
        _event: EventId,
        listener_id: u64,
        _mode: EventMode,
        payload: Vec<u8>,
        _next_token: Option<u64>,
    ) -> Result<EventReply, KernelError> {
        if listener_id == STARTED_LISTENER {
            let started = cordis_guest::decode::<RequestStarted>(&payload)?;
            let header_count = started.headers.len();
            host::log(
                context,
                "info",
                &format!(
                    "request started: {} {} ({} headers)",
                    started.method, started.path, header_count
                ),
            );
        } else if listener_id == FINISHED_LISTENER {
            let finished = cordis_guest::decode::<RequestFinished>(&payload)?;
            host::log(
                context,
                "info",
                &format!(
                    "request finished: status={} bytes={} duration={}ms",
                    finished.status, finished.bytes, finished.duration_ms
                ),
            );
        }
        Ok(EventReply::ContinueValue(payload))
    }
}

cordis_guest::export_plugin!(LogPlugin);
