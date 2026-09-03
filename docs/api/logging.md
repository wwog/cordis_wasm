# Logging (`cordis-logger`)

`cordis-logger` is a structured, cloneable logging service with a bounded in-memory history and
effect-owned exporters. It is deliberately a **parallel** application logging service, not a
`tracing-subscriber` implementation: runtime adapters (such as the Wasm kernel's `log`) may emit the
same event to both systems, but installing a subscriber that feeds records *back* into `Logger` would
risk recursion and duplicate delivery. That tradeoff is declared at the top of the crate.

## `LogLevel`

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Trace, Debug, Info, Warn, Error,
}
```

Ordering is `Trace < Debug < Info < Warn < Error`, so `level < minimum` filters records below a
configured threshold. The Wasm kernel maps guest level strings to these ('error'→Error, 'warn'→Warn,
'debug'→Debug, 'trace'→Trace, anything else→Info).

## `LogRecord`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    pub sequence: u64,
    pub level: LogLevel,
    pub target: Arc<str>,
    pub message: Arc<str>,
    pub fiber: Option<FiberId>,
}
```

One emitted record. `sequence` is the monotonically increasing record number; `target` is the logger
name/scope (e.g. `"cordis.guest"`); `fiber` is set when the record came from a specific component
fiber. Records are `Clone`, so an exporter can hold a copy.

## `LogExporter`

```rust
pub trait LogExporter: Send + Sync + 'static {
    fn export(&self, record: &LogRecord);
}
```

An exporter that receives every emitted record. It is **synchronous** and must not block — the logger
calls each exporter in turn after recording the event.

## `ConsoleExporter`

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct ConsoleExporter;
impl LogExporter for ConsoleExporter { /* eprintln! per record */ }
```

Plain stderr exporter kept outside `cordis-core`. Prints `[level] [target] [fiber=N] message` to
stderr. The CLI's `run` command registers one on the driver's logger.

## `Logger`

```rust
#[derive(Clone)]
pub struct Logger { /* state: Arc<Mutex<LoggerState>> */ }
impl Logger {
    pub fn new(capacity: usize) -> Self;
    pub fn set_level(&self, target: impl Into<String>, minimum: LogLevel);
    pub fn register_exporter(&self, exporter: Arc<dyn LogExporter>, scope: &EffectScope) -> Result<u64, CordisError>;
    pub fn log(&self, level: LogLevel, target: impl Into<Arc<str>>, message: impl Into<Arc<str>>, fiber: Option<FiberId>);
    pub fn records(&self) -> Vec<LogRecord>;
}
```

A cloneable structured logger with fixed-capacity history. Cloning shares the same `Arc<Mutex<...>>`
state.

### `new(capacity)`

Creates a logger with a ring buffer of `capacity` records. `capacity == 0` disables history (records
are still emitted to exporters, but not stored).

### `set_level(target, minimum)`

Sets a minimum `LogLevel` for a `target` prefix. The most-specific matching prefix wins: when logging
to `"app.db.query"` with `"app"`→Warn and `"app.db"`→Debug set, `"app.db"` (longest prefix) is used.
Test: `longest_target_filter_wins`.

### `register_exporter(exporter, scope)`

Registers an exporter and **owns its removal by `scope`**. A disposer is deferred into the scope that
removes the exporter when the scope disposes, so an exporter is torn down with the fiber that created
it. Returns the exporter id.

**Errors** — `CordisError::InactiveEffect` when the scope is disposing.

### `log(level, target, message, fiber)`

Emits one record. Steps:

1. Resolve the minimum level for `target` (longest matching prefix, default `Info`).
2. Drop the record if `level < minimum`.
3. Assign a sequence number and (if `capacity > 0`) push to the bounded ring, evicting the oldest.
4. Call every exporter with the record.

### `records()`

Returns a snapshot of the stored history, oldest first.

## Bounded-history ring

The buffer is a `VecDeque<LogRecord>` with a fixed capacity. When full, logging pops the front before
pushing the back, so only the last `capacity` records are retained. Test: `buffer_is_bounded_and_exporter_is_effect_owned`
logs 3 records into a capacity-2 logger and asserts `records().len() == 2`.

## Effect-owned exporters

`register_exporter` defers the exporter removal as a `Disposer` into the given `EffectScope`. In the
same test, disposing the owner stops the exporter: subsequent `log` calls are not delivered to the
(captured) exporter. This is why the CLI's `run` registers the `ConsoleExporter` against a
`EffectGuard` it disposes on shutdown.

## Runtime adapters

The Wasm kernel router (`RuntimeKernel::log`, see [wasm-driver](wasm-driver.md)) maps guest levels to
`LogLevel` and emits to both the `Logger` (`target = "cordis.guest"`) and `tracing` — exactly the
"parallel application logging service" the crate advertises.
