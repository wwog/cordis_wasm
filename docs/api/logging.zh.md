# 日志（`cordis-logger`）

`cordis-logger` 是一个结构化、可克隆的日志服务，带有有界的内存历史与随 effect（effect-owned）的 exporters。它有意作为一个**并行（parallel）**的应用日志服务，而不是一个 `tracing-subscriber` 实现：运行时适配器（如 Wasm kernel 的 `log`）可以把同一事件同时发往两套系统，但如果安装一个把记录*回灌*到 `Logger` 的 subscriber，就会有递归与重复投递的风险。这一取舍在 crate 顶部有说明。

## `LogLevel`

```rust
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Trace, Debug, Info, Warn, Error,
}
```

排序是 `Trace < Debug < Info < Warn < Error`，因此 `level < minimum` 会过滤掉低于所配置阈值的记录。Wasm kernel 把 guest 的级别字符串映射到这些值（'error'→Error、'warn'→Warn、'debug'→Debug、'trace'→Trace，其他一律→Info）。

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

一条已发出的记录。`sequence` 是单调递增的记录号；`target` 是 logger 名称/scope（例如 `"cordis.guest"`）；当记录来自某个特定组件的 fiber 时，`fiber` 会被设置。记录是 `Clone` 的，因此 exporter 可以持有一份副本。

## `LogExporter`

```rust
pub trait LogExporter: Send + Sync + 'static {
    fn export(&self, record: &LogRecord);
}
```

接收每一条已发出记录的 exporter。它是**同步**的且不得阻塞——logger 在记录事件后逐一调用每个 exporter。

## `ConsoleExporter`

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct ConsoleExporter;
impl LogExporter for ConsoleExporter { /* eprintln! per record */ }
```

放在 `cordis-core` 之外的普通 stderr exporter。把 `[level] [target] [fiber=N] message` 打印到 stderr。CLI 的 `run` 命令会在 driver 的 logger 上注册一个。

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

一个带固定容量历史的可克隆结构化 logger。克隆会共享同一个 `Arc<Mutex<...>>` 状态。

### `new(capacity)`

创建一个带 `capacity` 条记录环形缓冲的 logger。`capacity == 0` 会禁用历史（记录仍会发往 exporters，但不会被存储）。

### `set_level(target, minimum)`

为某个 `target` 前缀设置最低 `LogLevel`。最具体的匹配前缀胜出：当日志写到 `"app.db.query"`，且 `"app"`→Warn、`"app.db"`→Debug 都已设置时，使用 `"app.db"`（最长前缀）。测试：`longest_target_filter_wins`。

### `register_exporter(exporter, scope)`

注册一个 exporter，并**由 `scope` 拥有其移除**。一个 disposer 会被 defer 进该 scope，当 scope 被 dispose 时移除该 exporter，因此 exporter 会随创建它的 fiber 一起被拆除（torn down）。返回 exporter id。

**错误** —— scope 正在 disposing 时返回 `CordisError::InactiveEffect`。

### `log(level, target, message, fiber)`

发出一条记录。步骤：

1. 解析 `target` 的最低级别（最长匹配前缀，默认 `Info`）。
2. 若 `level < minimum` 则丢弃该记录。
3. 分配一个序号，并（若 `capacity > 0`）push 进有界环形缓冲，逐出最旧的记录。
4. 用该记录调用每个 exporter。

### `records()`

返回已存储历史的快照，最旧的在前。

## 有界历史环形缓冲

该缓冲是一个固定容量的 `VecDeque<LogRecord>`。满时，logging 会先 pop 前端再 push 后端，因此只保留最后 `capacity` 条记录。测试 `buffer_is_bounded_and_exporter_is_effect_owned` 向一个容量为 2 的 logger 记录 3 条记录，并断言 `records().len() == 2`。

## effect 拥有的 exporters

`register_exporter` 把移除 exporter 的操作作为 `Disposer` defer 进给定的 `EffectScope`。在同一个测试中，dispose owner 会停止 exporter：随后的 `log` 调用不会被投递给（已捕获的）exporter。这就是为什么 CLI 的 `run` 会把 `ConsoleExporter` 注册到一个在关闭时 dispose 的 `EffectGuard` 上。

## 运行时适配器

Wasm kernel 路由器（`RuntimeKernel::log`，见 [wasm-driver](wasm-driver.zh.md)）把 guest 级别映射到 `LogLevel`，并同时发往 `Logger`（`target = "cordis.guest"`）与 `tracing`——正是 crate 宣称的"并行（parallel）应用日志服务"。
