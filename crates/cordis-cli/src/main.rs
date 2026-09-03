use cordis_loader::IncludeDocument;
use cordis_logger::ConsoleExporter;
use cordis_wasm::{ArtifactPolicy, HmrWatcher, WasmApplication, WasmLimits, check_entries};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

#[tokio::main]
async fn main() -> ExitCode {
    match execute(std::env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cordis: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn execute(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match arguments.as_slice() {
        [command, config] if command == "check" => check(Path::new(config)).await,
        [command, config] if command == "run" => run(Path::new(config)).await,
        [command, config] if command == "inspect" => inspect(Path::new(config)).await,
        [command, package] if command == "build-component" => build_component(package, false),
        [command, package, release] if command == "build-component" && release == "--release" => {
            build_component(package, true)
        }
        _ => Err(usage().into()),
    }
}

async fn check(config: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (base, entries) = load_entries(config)?;
    let report = check_entries(
        base,
        entries,
        WasmLimits::default(),
        ArtifactPolicy::default(),
    )
    .await?;
    println!(
        "ok: {} entries, {} components",
        report.entries,
        report.components.len()
    );
    for component in report.components {
        println!("  {component}");
    }
    Ok(())
}

async fn inspect(config: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (base, entries) = load_entries(config)?;
    let mut application =
        WasmApplication::new(base, WasmLimits::default(), ArtifactPolicy::default()).await?;
    application.reconcile(entries).await?;
    let snapshot = application.settle().await?;
    println!("fibers: {}", snapshot.fibers.len());
    for fiber in &snapshot.fibers {
        println!(
            "  fiber={} parent={:?} state={:?} dependencies={}",
            fiber.id,
            fiber.parent,
            fiber.state,
            fiber.desired.entries().len()
        );
    }
    application.shutdown().await?;
    Ok(())
}

async fn run(config: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (base, entries) = load_entries(config)?;
    let mut application =
        WasmApplication::new(base, WasmLimits::default(), ArtifactPolicy::default()).await?;
    application.reconcile(entries).await?;
    let (logging, logging_scope) = cordis_core::EffectGuard::new("console-exporter");
    application
        .driver()
        .logger()
        .register_exporter(std::sync::Arc::new(ConsoleExporter), &logging_scope)?;
    let snapshot = application.settle().await?;
    let paths = application.driver().artifact_paths().await;
    println!(
        "running {} fibers across {} artifacts; press Ctrl-C to stop",
        snapshot.fibers.len().saturating_sub(1),
        paths.len()
    );

    if paths.is_empty() {
        tokio::signal::ctrl_c().await?;
    } else {
        let watcher = HmrWatcher::new(paths, Duration::from_millis(150))?;
        let (sender, mut events) = tokio::sync::mpsc::channel(8);
        let watcher_thread = std::thread::spawn(move || {
            loop {
                if sender.is_closed() {
                    break;
                }
                match watcher.next_timeout(Duration::from_millis(250)) {
                    Ok(Some(paths)) => {
                        if sender.blocking_send(paths).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("cordis: HMR watcher failed: {error}");
                        break;
                    }
                }
            }
        });

        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    break;
                }
                event = events.recv() => {
                    let Some(paths) = event else { break };
                    let report = application.driver().reload_paths(paths).await;
                    if report.committed {
                        println!("hmr: committed {} entries", report.entries.len());
                    } else {
                        eprintln!("hmr: transaction rolled back: {:?}", report.entries);
                    }
                }
            }
        }
        drop(events);
        watcher_thread
            .join()
            .map_err(|_| "HMR watcher thread panicked")?;
    }

    let snapshot = application.shutdown().await?;
    logging.dispose().await?;
    println!("stopped {} fibers", snapshot.fibers.len());
    Ok(())
}

fn load_entries(
    config: &Path,
) -> Result<(PathBuf, Vec<cordis_loader::EntrySpec>), Box<dyn std::error::Error>> {
    let context = std::env::var("CORDIS_SHARED")
        .ok()
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or(Value::Null);
    let document = IncludeDocument::load(config, &[], &context)?;
    let base = config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    Ok((base, document.entries().to_vec()))
}

fn build_component(package: &str, release: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::new("cargo");
    command.args(["build", "--target", "wasm32-wasip2", "-p", package]);
    if release {
        command.arg("--release");
    }
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("component build exited with {status}").into())
    }
}

fn usage() -> &'static str {
    "usage: cordis check|run|inspect <config.{json,yaml}>\n       cordis build-component <package> [--release]"
}
