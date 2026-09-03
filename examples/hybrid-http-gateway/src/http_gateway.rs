//! Native HTTP gateway: a static component that provides the HTTP "basis" and
//! emits request lifecycle events for dynamic plugins to observe.
//!
//! This demonstrates the static/dynamic split: the gateway owns the real HTTP
//! engine (here simulated), while the WebAssembly log plugin only subscribes to
//! its events and records structured fields.

use cordis_core::{
    ComponentFactory, ComponentFuture, ComponentInstance, DynamicCall, DynamicComponentDescriptor,
    EventCall, EventId, EventMode, EventReply, InstanceHost, InjectSpec, ServiceId,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

// Wire protocol shared with the wasm-log-plugin guest.
const REQUEST_STARTED_ABI: [u8; 32] = [0xA1; 32];
const REQUEST_FINISHED_ABI: [u8; 32] = [0xB2; 32];
const STARTED_LISTENER: u64 = 1;
const FINISHED_LISTENER: u64 = 2;
const HTTP_ABI: [u8; 32] = [0x01; 32];
const HANDLE_METHOD: u32 = 1;
const REQUEST_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Clone)]
pub(crate) struct HttpGatewayFactory;

#[derive(Clone, Debug, Serialize)]
struct RequestStarted {
    path: String,
    method: String,
    headers: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize)]
struct RequestFinished {
    status: u16,
    bytes: usize,
    duration_ms: u64,
}

impl ComponentFactory for HttpGatewayFactory {
    fn descriptor(&self) -> &DynamicComponentDescriptor {
        static DESCRIPTOR: std::sync::OnceLock<DynamicComponentDescriptor> = std::sync::OnceLock::new();
        DESCRIPTOR.get_or_init(|| DynamicComponentDescriptor {
            name: "example.http-gateway".into(),
            version: "0.1.0".into(),
            kernel_abi: "0.1".into(),
            injects: Vec::<InjectSpec>::new(),
            provides: vec![http_service()],
            config_schema: true.into(),
            capabilities: BTreeSet::new(),
        })
    }

    fn instantiate(&self, host: InstanceHost) -> ComponentFuture<'_, Box<dyn ComponentInstance>> {
        Box::pin(async move { Ok(Box::new(HttpGatewayInstance { host }) as Box<dyn ComponentInstance>) })
    }
}

struct HttpGatewayInstance {
    host: InstanceHost,
}

impl ComponentInstance for HttpGatewayInstance {
    fn activate(&mut self, _config: Value) -> ComponentFuture<'_, ()> {
        let host = self.host.clone();
        Box::pin(async move {
            // Spawn the self-driving request loop. This is what makes the example
            // self-contained: the gateway periodically produces a request and
            // emits started/finished events for listeners to observe.
            //
            // The initial delay gives the supervisor time to activate the
            // WebAssembly log plugin and register its listeners before the first
            // event is dispatched; without it the first cycle would fail lookup.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                loop {
                    if let Err(error) = simulate_request(&host).await {
                        host.log("warn", &format!("request cycle dropped: {error}"));
                    }
                    tokio::time::sleep(REQUEST_INTERVAL).await;
                }
            });
            Ok(())
        })
    }

    fn deactivate(&mut self) -> ComponentFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn call_service(&mut self, call: DynamicCall) -> ComponentFuture<'_, Vec<u8>> {
        Box::pin(async move {
            if call.service != http_service() || call.method != HANDLE_METHOD {
                return Err(cordis_core::CordisError::ComponentFailed {
                    component: "example.http-gateway".into(),
                    message: "unknown service method".into(),
                });
            }
            // A real gateway would parse the HTTP request here. For the example we
            // synthesize a deterministic response so plugins can observe fields.
            let record = RequestFinished {
                status: 200,
                bytes: 128,
                duration_ms: 7,
            };
            let payload = cordis_core::encode_service_payload(&record)
                .map_err(|error| cordis_core::CordisError::ComponentFailed {
                    component: "example.http-gateway".into(),
                    message: error.to_string(),
                })?;
            Ok(payload)
        })
    }

    fn call_event(&mut self, call: EventCall) -> ComponentFuture<'_, EventReply> {
        Box::pin(async move {
            // This component emits (not listens to) lifecycle events, so it treats
            // an inbound event as invalid unless a plugin echoes it back.
            let _ = call;
            Ok(EventReply::Continue(Vec::new()))
        })
    }
}

/// One simulated request cycle: emit `started`, synthesize a response, emit
/// `finished`. The payloads follow the wire protocol understood by the guest.
async fn simulate_request(host: &InstanceHost) -> Result<(), cordis_core::CordisError> {
    let started = RequestStarted {
        path: "/hello".to_owned(),
        method: "GET".to_owned(),
        headers: vec![("user-agent".to_owned(), "cordis-example/1.0".to_owned())],
    };
    let payload = cordis_core::encode_event_payload(&started)?;
    host.dispatch_event(EventCall {
        event: started_event(),
        listener_id: STARTED_LISTENER,
        mode: EventMode::Serial,
        payload,
        next_token: None,
    })
    .await?;

    // A real gateway would now run the request logic. We simulate a short
    // handler so `duration_ms` is measured (and observably non-zero) rather than
    // hard-coded, keeping the log honest about what the example synthesizes.
    let begin = Instant::now();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let duration_ms = u64::try_from(begin.elapsed().as_millis()).map_err(|_| {
        cordis_core::CordisError::ComponentFailed {
            component: "example.http-gateway".into(),
            message: "elapsed duration out of range".into(),
        }
    })?;
    let payload = cordis_core::encode_event_payload(&RequestFinished {
        status: 200,
        bytes: 128,
        duration_ms,
    })?;
    host.dispatch_event(EventCall {
        event: finished_event(),
        listener_id: FINISHED_LISTENER,
        mode: EventMode::Serial,
        payload,
        next_token: None,
    })
    .await?;
    Ok(())
}

fn http_service() -> ServiceId {
    ServiceId::new("example.http", HTTP_ABI)
}

fn started_event() -> EventId {
    EventId::new("http.request.started", REQUEST_STARTED_ABI)
}

fn finished_event() -> EventId {
    EventId::new("http.request.finished", REQUEST_FINISHED_ABI)
}
