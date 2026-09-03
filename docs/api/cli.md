# CLI (`cordis-cli`)

`cordis-cli` is the `cordis` command-line tool. It reads a declarative application config
(`cordis.json` / `.yaml`), preflights the referenced components, optionally activates them, and hot
reloads on config or artifact change. There is no daemon: `run` is a foreground process that watches
files and reloads until you press Ctrl-C.

```
usage: cordis check|run|inspect <config.{json,yaml}>
       cordis build-component <package> [--release]
       cordis --help | --version
```

## Subcommands

| Command | What it does |
|---|---|
| `check <config>` | Preflight only. Validates the entry tree and every referenced component (compile, descriptor, WIT, capability, config schema) without activating anything. Prints `ok: N entries, M components` and the component names. |
| `run <config>` | Preflight, then activate, then watch config and artifacts for HMR. Runs until Ctrl-C. |
| `inspect <config>` | Preflight, activate, settle, then print the fiber snapshots. Good for seeing the resolved dependency graph. |
| `build-component <package> [--release]` | Build a guest crate to `wasm32-wasip2`. Runs `cargo build --target wasm32-wasip2 -p <package>` (adds `--release` when given). |
| `--help` / `-h` | Print usage. |
| `--version` / `-V` | Print `cordis 0.1.0`. |

Exit code is `0` on success, `1` on any error (printed as `cordis: <error>` to stderr).

## `check`

```rust
async fn check(config: &Path) -> Result<(), Box<dyn Error>> {
    let (base, entries) = load_entries(config)?;
    let report = check_entries(base, entries, WasmLimits::default(), ArtifactPolicy::default()).await?;
    println!("ok: {} entries, {} components", report.entries, report.components.len());
    for component in report.components { println!("  {component}"); }
    Ok(())
}
```

`check` never starts a Supervisor or instantiates a component. It uses `check_entries` with the default
`WasmLimits` and `ArtifactPolicy` (see [wasm-driver](wasm-driver.md)).

## `inspect`

```rust
async fn inspect(config: &Path) -> Result<(), Box<dyn Error>> {
    let (base, entries) = load_entries(config)?;
    let mut application = WasmApplication::new(base, WasmLimits::default(), ArtifactPolicy::default()).await?;
    application.reconcile(entries).await?;
    let snapshot = application.settle().await?;
    println!("fibers: {}", snapshot.fibers.len());
    for fiber in &snapshot.fibers {
        println!("  fiber={} parent={:?} state={:?} dependencies={}", fiber.id, fiber.parent, fiber.state, fiber.desired.entries().len());
    }
    application.shutdown().await?;
    Ok(())
}
```

`inspect` *does* activate (through `reconcile`) so it can print the real resolver state — fiber id,
parent, state, and the number of declared dependencies — then shuts down, leaving no running processes.

## `run` and the HMR watcher loop

`run` is the long-lived command. Its flow:

1. Load and canonicalize the config path.
2. Create a `WasmApplication`, reconcile the initial entries, register a `ConsoleExporter` on the
   driver's logger.
3. `settle()` to wait for all runnable lifecycle work.
4. Build the HMR target set: the artifact paths **plus the config path itself**.
5. Start a watcher thread running `watch_loop` (see below).
6. Loop on `tokio::select!` over Ctrl-C and the watch event channel:
   - If the **config path** changed: re-`load_entries` and `application.reconcile`. On success it
     `settle()`s, reconfigures the watcher to the new artifact set, and prints the active fiber count;
     on failure it prints "config: transaction rolled back" (the previous entry tree is preserved by
     the transactional reconcile).
   - If other **artifact** paths changed: `application.driver().reload_paths(paths)` and print
     "hmr: committed N entries" or "hmr: transaction rolled back".
7. On Ctrl-C: shut the watcher down, `application.shutdown()`, dispose the logging effect.

The `watch_loop` thread repeatedly drains `WatchCommand`s (replace the watcher target set, or shut
down) and then waits `next_timeout(250ms)` on the debounced filesystem watcher, forwarding each
debounced path set to the async loop. A watcher reconfiguration failure is logged but does not kill the
loop.

So `cordis run` watches **both** the config and the mounted artifacts, and treats a config diff as a
reversible batch transaction (failed config leaves the previous tree in place) — the README's
"simultaneously watches config and mounted Component; config diff is a reversible batch transaction,
failure keeps the previous Entry Tree."

## `build-component`

```rust
fn build_component(package: &str, release: bool) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command.args(["build", "--target", "wasm32-wasip2", "-p", package]);
    if release { command.arg("--release"); }
    ...
}
```

Builds a guest crate to `wasm32-wasip2`. The output `target/wasm32-wasip2/{debug,release}/<crate>.wasm`
is what a `file:` Entry references.

## `CORDIS_SHARED`

`load_entries` reads the `CORDIS_SHARED` environment variable and, if present, parses it as JSON to use
as the `ctx` value for `!expr` expressions in YAML includes:

```rust
let context = std::env::var("CORDIS_SHARED")
    .ok()
    .map(|value| serde_json::from_str(&value))
    .transpose()?
    .unwrap_or(Value::Null);
```

So an embedder can inject runtime context into a declarative config's YAML expressions without
editing the file (e.g. a port number or a registry value). If unset, `ctx` is `null`.

## The `cordis.json` entry format

`load_entries` uses `IncludeDocument::load`, so the config is an entry array or an object with an
`entries` array, and may be JSON or YAML with `!expr` tags. See [config](config.md) for the full
file syntax (entry fields, `isolate`/`intercept`, component refs, config schemas, and `!expr`), and
[loader](loader.md) / [wasm-driver](wasm-driver.md) for the loader types.

## Parsing contract

The argument parser is exact: `check`/`run`/`inspect` require exactly `[command, config]`;
`build-component` requires `[command, package]` or `[command, package, "--release"]`; a bare
`--help`/`-h` or `--version`/`-V` is recognized; anything else prints usage and fails. Tests:
`stable_commands_parse_to_typed_contract`, `help_version_and_invalid_shapes_are_unambiguous`.
