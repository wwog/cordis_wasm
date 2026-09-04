//! Minimal embedding host: wires a native HTTP gateway (`builtin:`) and a
//! WebAssembly log plugin (`file:`) into one application, driven entirely by a
//! declarative `cordis.json`.
//!
//! This is the config-driven counterpart to `hybrid-http-gateway` (which hard-
//! codes its topology in Rust). Here the topology — which components run, with
//! what config, and how they connect — lives in `cordis.json`, and the host does
//! only two things: register the process-local factories, and load + reconcile
//! the config file.
//!
//! ## Why `builtin:` still needs a `BuiltinRegistry`
//!
//! The config file only *references* a builtin by name:
//!
//! ```json
//! { "id": "gateway", "component": "builtin:http-gateway", "config": { "interval_ms": 1000 } }
//! ```
//!
//! `buildin:` names a factory that must already be registered *in code*. The
//! registry (`BuiltinRegistry::register`) declares **what components exist**;
//! the config file declares **how they are wired together** (config values,
//! realm isolation, tree nesting, enabled/disabled). Neither is redundant:
//! without the registry, `builtin:http-gateway` would fail to resolve; without
//! the config, the registry has nothing to instantiate.

use cordis_loader::IncludeDocument;
use cordis_logger::{ConsoleExporter, LogLevel};
use cordis_wasm::{ArtifactPolicy, BuiltinRegistry, WasmApplication, WasmLimits};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod gateway;
use gateway::HttpGatewayFactory;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // 1. Programmatically declare which process-local factories exist. This is
    //    the part that can only be done in code — the `.wasm` plugin needs no
    //    such step, because `file:` loads the artifact directly.
    let builtins = BuiltinRegistry::default();
    builtins.register("http-gateway", Arc::new(HttpGatewayFactory))?;

    // 2. Load the application topology from the config file. This is where the
    //    static/dynamic combination is declared: `buildin:http-gateway` plus a
    //    `file:` wasm artifact, both in one tree.
    let config_path = base_dir.join("cordis.json");
    let document = IncludeDocument::load(&config_path, &[], &Value::Null)?;
    let entries = document.entries().to_vec();

    // 3. Build the application.
    let mut app = WasmApplication::new_with_builtins(
        &base_dir,
        WasmLimits::default(),
        ArtifactPolicy::default(),
        builtins,
    )
    .await?;

    // Route guest `host::log` calls to stderr. The exporter is registered before
    // the first (delayed) request cycle, so every lifecycle record it emits is
    // captured. The guard (`_logging`) must live until shutdown so the effect
    // scope stays armed.
    let (_logging, logging_scope) = cordis_core::EffectGuard::new("console-exporter");
    app.driver()
        .logger()
        .register_exporter(Arc::new(ConsoleExporter), &logging_scope)?;

    // Enable framework debug logs. Internal lifecycle traces (compilation, entry
    // mounting, fiber activation) are emitted under the `cordis` target and only
    // surface when the logger is filtered down to `Debug`. Set this *before*
    // reconcile so the mount-time traces are captured.
    app.driver().logger().set_level("cordis", LogLevel::Debug);

    app.reconcile(entries).await?;

    // 4. Let the app settle, observe the wiring, then run a few request cycles.
    let snapshot = app.settle().await?;
    let active = snapshot
        .fibers
        .iter()
        .filter(|fiber| cordis_core::FiberState::Active == fiber.state)
        .count();
    eprintln!(
        "{} [host] {active} fibers active; running for 3 request cycles…",
        ts()
    );

    tokio::time::sleep(Duration::from_secs(3)).await;

    let snapshot = app.snapshot().await?;
    for fiber in &snapshot.fibers {
        eprintln!(
            "{} [host] fiber {:?} state={:?}",
            ts(),
            fiber.id,
            fiber.state
        );
    }

    app.shutdown().await?;
    eprintln!("{} [host] shutdown complete", ts());
    Ok(())
}

/// Wall-clock timestamp `HH:MM:SS.mmm` for `[host]` diagnostics, so host-side
/// output can be interleaved with the millisecond-resolution cycle logs from
/// the guest (which arrive via `ConsoleExporter` without a timestamp).
fn ts() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_millis = now.as_millis();
    let millis = total_millis % 1000;
    let total_seconds = total_millis / 1000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = (total_seconds / 3600) % 24;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}
