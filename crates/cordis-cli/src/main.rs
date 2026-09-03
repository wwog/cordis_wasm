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
    match parse_arguments(&arguments)? {
        CliCommand::Check(config) => check(&config).await,
        CliCommand::Run(config) => run(&config).await,
        CliCommand::Inspect(config) => inspect(&config).await,
        CliCommand::BuildComponent { package, release } => build_component(&package, release),
        CliCommand::Help => {
            println!("{}", usage());
            Ok(())
        }
        CliCommand::Version => {
            println!("cordis {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CliCommand {
    Check(PathBuf),
    Run(PathBuf),
    Inspect(PathBuf),
    BuildComponent { package: String, release: bool },
    Help,
    Version,
}

fn parse_arguments(arguments: &[String]) -> Result<CliCommand, &'static str> {
    match arguments {
        [flag] if flag == "--help" || flag == "-h" => Ok(CliCommand::Help),
        [flag] if flag == "--version" || flag == "-V" => Ok(CliCommand::Version),
        [command, config] if command == "check" => Ok(CliCommand::Check(config.into())),
        [command, config] if command == "run" => Ok(CliCommand::Run(config.into())),
        [command, config] if command == "inspect" => Ok(CliCommand::Inspect(config.into())),
        [command, package] if command == "build-component" => Ok(CliCommand::BuildComponent {
            package: package.clone(),
            release: false,
        }),
        [command, package, release] if command == "build-component" && release == "--release" => {
            Ok(CliCommand::BuildComponent {
                package: package.clone(),
                release: true,
            })
        }
        _ => Err(usage()),
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
    let config_path = config.canonicalize()?;
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

    let mut targets = paths;
    targets.push(config_path.clone());
    let watcher = HmrWatcher::new(targets, Duration::from_millis(150))?;
    let (watch_commands, watch_command_receiver) = std::sync::mpsc::channel();
    let (sender, mut events) = tokio::sync::mpsc::channel(8);
    let watcher_thread = std::thread::spawn(move || {
        watch_loop(watcher, watch_command_receiver, sender);
    });

    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break;
            }
            event = events.recv() => {
                let Some(mut paths) = event else { break };
                if paths.iter().any(|path| path == &config_path) {
                    paths.retain(|path| path != &config_path);
                    match load_entries(&config_path) {
                        Ok((_, entries)) => match application.reconcile(entries).await {
                            Ok(()) => {
                                let snapshot = application.settle().await?;
                                let mut targets = application.driver().artifact_paths().await;
                                targets.push(config_path.clone());
                                watch_commands.send(WatchCommand::Replace(targets))?;
                                println!(
                                    "config: committed {} active fibers",
                                    snapshot.fibers.len().saturating_sub(1)
                                );
                            }
                            Err(error) => eprintln!("config: transaction rolled back: {error}"),
                        },
                        Err(error) => eprintln!("config: preflight failed: {error}"),
                    }
                }
                if !paths.is_empty() {
                    let report = application.driver().reload_paths(paths).await;
                    if report.committed {
                        println!("hmr: committed {} entries", report.entries.len());
                    } else {
                        eprintln!("hmr: transaction rolled back: {:?}", report.entries);
                    }
                }
            }
        }
    }
    let _ = watch_commands.send(WatchCommand::Shutdown);
    drop(events);
    watcher_thread
        .join()
        .map_err(|_| "HMR watcher thread panicked")?;

    let snapshot = application.shutdown().await?;
    logging.dispose().await?;
    println!("stopped {} fibers", snapshot.fibers.len());
    Ok(())
}

enum WatchCommand {
    Replace(Vec<PathBuf>),
    Shutdown,
}

fn watch_loop(
    mut watcher: HmrWatcher,
    commands: std::sync::mpsc::Receiver<WatchCommand>,
    sender: tokio::sync::mpsc::Sender<Vec<PathBuf>>,
) {
    loop {
        match commands.try_recv() {
            Ok(WatchCommand::Replace(paths)) => {
                match HmrWatcher::new(paths, Duration::from_millis(150)) {
                    Ok(replacement) => watcher = replacement,
                    Err(error) => eprintln!("cordis: watcher reconfiguration failed: {error}"),
                }
            }
            Ok(WatchCommand::Shutdown) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match watcher.next_timeout(Duration::from_millis(250)) {
            Ok(Some(paths)) => {
                if sender.blocking_send(paths).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("cordis: watcher failed: {error}");
                break;
            }
        }
    }
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
    "usage: cordis check|run|inspect <config.{json,yaml}>\n       cordis build-component <package> [--release]\n       cordis --help | --version"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn stable_commands_parse_to_typed_contract() {
        assert_eq!(
            parse_arguments(&args(&["check", "cordis.json"])),
            Ok(CliCommand::Check("cordis.json".into()))
        );
        assert_eq!(
            parse_arguments(&args(&["run", "cordis.yaml"])),
            Ok(CliCommand::Run("cordis.yaml".into()))
        );
        assert_eq!(
            parse_arguments(&args(&["inspect", "cordis.json"])),
            Ok(CliCommand::Inspect("cordis.json".into()))
        );
        assert_eq!(
            parse_arguments(&args(&["build-component", "guest", "--release"])),
            Ok(CliCommand::BuildComponent {
                package: "guest".into(),
                release: true,
            })
        );
    }

    #[test]
    fn help_version_and_invalid_shapes_are_unambiguous() {
        assert_eq!(parse_arguments(&args(&["-h"])), Ok(CliCommand::Help));
        assert_eq!(
            parse_arguments(&args(&["--version"])),
            Ok(CliCommand::Version)
        );
        assert_eq!(parse_arguments(&[]), Err(usage()));
        assert_eq!(
            parse_arguments(&args(&["build-component", "guest", "--debug"])),
            Err(usage())
        );
    }
}
