# Cordis (Rust) 教程

Cordis 是一个构建于**可逆 effect** 模型之上的 Rust 插件运行时：插件执行的每个 effect 都配有一个逆，运行时会在该插件卸载时以 LIFO 顺序应用这些逆。它在之上叠加了**响应式依赖注入**——插件只在其声明的每个服务都被真正提供时才激活。动态插件运行在 **Wasmtime 的 Component Model** 内，所以 guest 代码是一个 WebAssembly 组件，它通过带版本的 WIT kernel 接口与 host 对话，而不是经由 Rust ABI。

本教程讲的是如何编写那些动态插件：一个 *提供* 服务、*消费* 另一个服务、*响应* 事件，并由声明式的 `cordis.json` 文件组合进应用的 WebAssembly 组件。学完之后你将写出两个真正的插件——一个暴露 HTTP 风格的 web 服务，一个监听事件并呈现结果——并且能确切知道运行时的真正支持到哪里为止、哪里仍只是雏形。

读者对象是想要理解本仓库插件模型的 agent 或插件开发者。你不需要先掌握 WebAssembly Component Model，但应当能轻松地读 Rust、并跟得上 host/guest 边界。TypeScript 姊妹项目 Cordis（[`ts-docs/cordis-tutorial`](../ts-docs/cordis-tutorial/index.zh.md)）是本运行时的概念鼻祖；本教程在语义对齐的地方照应其结构，在对不齐的地方分叉。

## 本教程不是什么

这不是 Rust 语言指南，也不是 Wasmtime 教程。它假定你能用 Cargo 构建项目。它*诚实地*看待运行时：凡是尚属雏形的特性——HTTP 服务器支持和任何"view（视图）"概念是两大块——各章都会明说，而不是凭空发明 API。属于草图而非可用 crate 的代码会标注为 **illustrative（示意）**，且每个这样的代码块都说明哪些部分是真实的、哪些必须由你补齐。

## 前置条件

你需要一套可用的 Rust 工具链，并装好 WebAssembly Component Model 目标。

- **Rust 1.98**（workspace 锁定 `rust-version = "1.98"`，`rust-toolchain.toml` 选择 `channel = "1.98.0"`，带 `clippy` 和 `rustfmt`）。
- **`wasm32-wasip2`** 目标。guest 组件编译到 WASIp2，而不是较老的 `wasm32-wasi`：

  ```sh
  rustup target add wasm32-wasip2
  ```

你可以用 `rustup target list --installed` 确认它已安装；那一行应包含 `wasm32-wasip2`。

- **`cargo`**，以及——本教程要构建的产物所需要的——`wasm32-wasip2` 的 `cargo build` 支持，它随上面的工具链一起提供。本教程中的任何命令都不需要网络访问；这里的每个插件都是按路径加载的本地组件。

你**不需要**全局安装 `cordis`。你通过 Cargo 运行它：`cargo run -p cordis-cli -- <subcommand> <config>`。

## 你要在其中工作的目录

本仓库是一个 Cargo workspace。你关心的部分集中在少数几个地方：

| 路径 | 是什么 |
|---|---|
| `crates/cordis-guest/` | guest SDK：生成的 kernel WIT 绑定、`Guest` trait，以及需要你实现的辅助（helper）导出。 |
| `crates/cordis-guest/wit/kernel.wit` | guest 组件所实现的 WIT kernel world。host 副本位于 `crates/cordis-wasm/wit/kernel.wit`；有一个测试断言两者逐字节相同。 |
| `crates/cordis-wasm/` | host 侧：Wasmtime engine、limits、factory、capability 策略、loader driver，以及把调用路由到活动 fiber 的 kernel host。 |
| `crates/cordis-cli/` | `cordis` 命令：`check`、`inspect`、`run`、`build-component`。 |
| `examples/wasm-counter-provider/` | 一个 *提供* counter 服务的完整 guest。第 6 章中你的模板。 |
| `examples/wasm-counter-consumer/` | 一个 *注入* 同一 counter 服务并在 `activate` 期间调用它的完整 guest。第 7 章中你的模板。 |
| `examples/wasm-app/cordis.json` | 把两个示例组合起来的声明式应用。 |
| `docs/api/index.md` | API 参考索引。它列出的各 crate 页面正是各章深入阅读时指向的地方。 |

在本教程的核心章节里你不会新建顶层 crate——你会先读 `examples/`，然后在它们旁边写你自己的 guest。由于 workspace 会把 guest 示例作为 `xtask` 的一部分构建，把自己的 guest crate 放在 `examples/`（或作为并行的 workspace members）是最省事的路子。

## 如何运行参考示例

随附的两个 guest，`wasm-counter-provider` 和 `wasm-counter-consumer`，是本教程所教内容的全链路检验。在你写任何东西之前先运行它们一次，好让加载回路变得熟悉：

```sh
cargo run -p xtask -- build-guests
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
cargo run -p cordis-cli -- inspect examples/wasm-app/cordis.json
cargo run -p cordis-cli -- run examples/wasm-app/cordis.json
```

第一个命令把两个 guest 组件构建到 `target/wasm32-wasip2/debug/`，然后对它们运行 host 侧集成测试。`check` 校验声明和每个组件，但*不激活*其中任何一个。`inspect` 更进一步，挂载每个 entry，然后报告由此产生的 fiber tree。`run` 做上面所有事、注册一个 console log exporter，然后监视配置和 artifact 的热变更，直到你按 Ctrl-C。

第 1 章会逐行走一遍这些命令。

## 各章

1. [Your first run](01-first-plugin.zh.md) — 运行随附的 counter 示例并阅读它的 `cordis.json`。
2. [Anatomy of a guest plugin](02-plugin-anatomy.zh.md) — `Guest` trait、`PluginDescriptor`、`export_plugin!` 与 `CallContext`。
3. [Services and injection](03-services-and-inject.zh.md) — `provide`/`inject`、`ServiceId` 与 ABI hash、realms，以及与原生宏的对照。
4. [Events](04-events.zh.md) — WIT event 表面、五种 `EventMode`、`EventReply`，以及 listener 注册。
5. [Configuration and the sandbox](05-config-and-capabilities.zh.md) — config 字节、JSON Schema、`ArtifactPolicy`、`WasiCapabilities`、`WasmLimits`。
6. [Writing a web server plugin](06-writing-a-web-server-plugin.zh.md) — 主练习：一个提供 HTTP 服务的插件，以及真实的网络 capability 边界。
7. [Events and views](07-events-and-views.zh.md) — 第二个主练习：一个响应事件、并把结果呈现在可见之处的插件。
8. [Troubleshooting](08-troubleshooting.zh.md) — 常见失败模式以及如何解读它们。

## API 参考在哪里

完整的 API 参考位于 [the API index](../api/index.zh.md)。每个 crate 在 `../api/` 下都有自己的页面（`guest.md`、`service.md`、`event.md`、`wasm.md`、`loader.md` 等等）；索引列出每个页面及它所记录的公开条目。当你想看签名、`# Errors` 一节，或 walkthrough 一带而过的更深模型时，各章会链接到对应页面。

语义基础——本实现如何落到论文 *A Programming Paradigm for Spatiotemporal Composability* 上——在 [semantics.md](../semantics.zh.md)。它是行为*为何*存在的权威来源；本教程是*如何使用*的来源。

下一步：[Your first run](01-first-plugin.zh.md) — 运行 counter 示例并阅读它的 `cordis.json`。
