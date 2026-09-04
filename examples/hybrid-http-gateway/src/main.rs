//! Host executable: wires a native HTTP gateway (as a `builtin:`) together with
//! a WebAssembly log plugin into one declarative application, then runs it.
//!
//! This is the "static + dynamic" combination in action: the gateway is a
//! process-local factory registered through `BuiltinRegistry`, the log plugin is
//! a `.wasm` artifact referenced by `file:`, and both share the same Supervisor
//! lifecycle and kernel routing.

use cordis_loader::EntrySpec;
use cordis_logger::ConsoleExporter;
use cordis_wasm::{ArtifactPolicy, BuiltinRegistry, WasmApplication, WasmLimits};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod http_gateway;
use http_gateway::HttpGatewayFactory;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // Register the static HTTP gateway as a process-local `builtin:` factory.
    let builtins = BuiltinRegistry::default();
    builtins.register("http-gateway", Arc::new(HttpGatewayFactory))?;

    // The dynamic log plugin is compiled from a `.wasm` artifact (see xtask).
    let plugin_path = base_dir
        .join("../../target/wasm32-wasip2/debug/wasm_log_plugin.wasm")
        .canonicalize()
        .map_err(|error| {
            format!("log plugin not built; run `cargo run -p xtask -- build-guests` first: {error}")
        })?;

    let mut gateway = EntrySpec::leaf("http-gateway", "builtin:http-gateway")?;
    gateway.config = serde_json::json!({});
    let mut plugin = EntrySpec::leaf("log-plugin", format!("file:{}", plugin_path.display()))?;
    plugin.config = serde_json::json!({});

    let mut app = WasmApplication::new_with_builtins(
        &base_dir,
        WasmLimits::default(),
        ArtifactPolicy::default(),
        builtins,
    )
    .await?;
    app.reconcile(vec![plugin, gateway]).await?;

    // Route guest `host::log` calls to stderr. The exporter is registered before
    // the gateway's first (delayed) request cycle, so every lifecycle record it
    // emits is captured. The guard (`_logging`) must live until shutdown so the
    // effect scope stays armed.
    let (_logging, logging_scope) = cordis_core::EffectGuard::new("console-exporter");
    app.driver()
        .logger()
        .register_exporter(Arc::new(ConsoleExporter), &logging_scope)?;

    // Let the app reach quiescence (all fibers Active), then observe the wiring
    // before letting the gateway's self-driving loop run for a few cycles.
    let snapshot = app.settle().await?;
    let active = snapshot
        .fibers
        .iter()
        .filter(|fiber| cordis_core::FiberState::Active == fiber.state)
        .count();
    eprintln!("[host] {active} fibers active; running for 3 request cycles…");

    tokio::time::sleep(Duration::from_secs(3)).await;

    let snapshot = app.snapshot().await?;
    for fiber in &snapshot.fibers {
        eprintln!("[host] fiber {:?} state={:?}", fiber.id, fiber.state);
    }

    app.shutdown().await?;
    eprintln!("[host] shutdown complete");
    Ok(())
}
