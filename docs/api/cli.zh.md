# CLI（`cordis-cli`）

`cordis-cli` 是 `cordis` 命令行工具。它读取声明式的应用配置（`cordis.json` / `.yaml`），对引用的组件做预检，可选地激活它们，并在配置或 artifact 变化时热重载。没有 daemon：`run` 是一个前台进程，监视文件并重载，直到你按下 Ctrl-C。

```
usage: cordis check|run|inspect <config.{json,yaml}>
       cordis build-component <package> [--release]
       cordis --help | --version
```

## 子命令

| 命令 | 作用 |
|---|---|
| `check <config>` | 仅预检。校验 entry tree 及每一个被引用的组件（编译、descriptor、WIT、capability、config schema），但不激活任何东西。打印 `ok: N entries, M components` 与组件名。 |
| `run <config>` | 预检，然后激活，再监视 config 与 artifacts 以进行 HMR（热模块替换）。一直运行到 Ctrl-C。 |
| `inspect <config>` | 预检、激活、settle，然后打印 fiber 快照。适合查看解析出的依赖图。 |
| `build-component <package> [--release]` | 把 guest crate 构建到 `wasm32-wasip2`。运行 `cargo build --target wasm32-wasip2 -p <package>`（提供 `--release` 时加上它）。 |
| `--help` / `-h` | 打印用法。 |
| `--version` / `-V` | 打印 `cordis 0.1.0`。 |

成功时退出码为 `0`，任何错误为 `1`（以 `cordis: <error>` 打印到 stderr）。

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

`check` 从不启动 Supervisor，也不实例化任何组件。它使用默认的 `WasmLimits` 与 `ArtifactPolicy` 调用 `check_entries`（见 [wasm-driver](wasm-driver.zh.md)）。

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

`inspect` *确实*会激活（通过 `reconcile`），因此它能打印真实的 resolver 状态——fiber id、parent、state 以及声明的依赖数量——然后关闭，不留下任何运行中的进程。

## `run` 与 HMR watcher 循环

`run` 是长期运行的命令。它的流程：

1. 加载并规范化（canonicalize）config 路径。
2. 创建一个 `WasmApplication`，reconcile 初始 entries，并在 driver 的 logger 上注册一个 `ConsoleExporter`。
3. 调用 `settle()` 等待所有可运行的 lifecycle 工作完成。
4. 构建 HMR 目标集合：artifact 路径**加上 config 路径本身**。
5. 启动一个运行 `watch_loop` 的 watcher 线程（见下文）。
6. 在 Ctrl-C 与 watch 事件 channel 上用 `tokio::select!` 循环：
   - 如果 **config 路径**变了：重新 `load_entries` 并 `application.reconcile`。成功时它会 `settle()`、把 watcher 重新配置到新的 artifact 集合，并打印活跃 fiber 数；失败时打印 "config: transaction rolled back"（先前的 entry tree 由事务性 reconcile 保留）。
   - 如果其他 **artifact** 路径变了：调用 `application.driver().reload_paths(paths)` 并打印 "hmr: committed N entries" 或 "hmr: transaction rolled back"。
7. 按下 Ctrl-C 时：关闭 watcher、调用 `application.shutdown()`、dispose 日志 effect。

`watch_loop` 线程反复排空 `WatchCommand`（替换 watcher 的目标集合，或关闭），然后在防抖后的文件系统 watcher 上等待 `next_timeout(250ms)`，把每组防抖后的路径转发给异步循环。watcher 重新配置失败会被记录日志，但不会终止循环。

因此 `cordis run` **同时**监视 config 与已挂载的 artifacts，并把 config 差异当作可逆的批量事务（失败的 config 会让先前的 tree 保持不变）——正如 README 所说："simultaneously watches config and mounted Component; config diff is a reversible batch transaction, failure keeps the previous Entry Tree."

## `build-component`

```rust
fn build_component(package: &str, release: bool) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command.args(["build", "--target", "wasm32-wasip2", "-p", package]);
    if release { command.arg("--release"); }
    ...
}
```

把 guest crate 构建到 `wasm32-wasip2`。输出 `target/wasm32-wasip2/{debug,release}/<crate>.wasm` 正是 `file:` 类型的 Entry 所引用的东西。

## `CORDIS_SHARED`

`load_entries` 读取 `CORDIS_SHARED` 环境变量；若存在，则把它解析为 JSON，用作 YAML include 中 `!expr` 表达式的 `ctx` 值：

```rust
let context = std::env::var("CORDIS_SHARED")
    .ok()
    .map(|value| serde_json::from_str(&value))
    .transpose()?
    .unwrap_or(Value::Null);
```

因此嵌入方可以把运行时 context 注入声明式配置的 YAML 表达式，而无需编辑文件（例如端口号或注册表值）。若未设置，则 `ctx` 为 `null`。

## `cordis.json` 条目格式

`load_entries` 使用 `IncludeDocument::load`，因此 config 是一个 entry 数组，或者是一个带 `entries` 数组的对象，并且可以是带 `!expr` 标签的 JSON 或 YAML。完整的文件语法（entry 字段、`isolate`/`intercept`、组件引用、config schema 与 `!expr`）见 [config](config.zh.md)，loader 类型见 [loader](loader.zh.md) 与 [wasm-driver](wasm-driver.zh.md)。

## 解析契约

参数解析器是精确的：`check`/`run`/`inspect` 恰好要求 `[command, config]`；`build-component` 要求 `[command, package]` 或 `[command, package, "--release"]`；裸的 `--help`/`-h` 或 `--version`/`-V` 会被识别；任何其他内容都打印 usage 并失败。测试：`stable_commands_parse_to_typed_contract`、`help_version_and_invalid_shapes_are_unambiguous`。
