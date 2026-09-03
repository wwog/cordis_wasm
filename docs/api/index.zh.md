# Cordis (Rust) — API 参考

Cordis 是一个基于**可逆 effect**模型的 Rust 插件运行时：组件执行的每个 effect 都配有一个逆，运行时在所属 fiber 卸载时按 LIFO 顺序应用这些逆。它还加入了**响应式依赖注入**——组件只在其声明的服务真正被提供时才激活——并把 **Wasmtime Component Model** 作为其唯一的动态插件机制。语义在论文 `2608.25512v1.txt` 中规定，并在 [semantics.zh.md](../semantics.zh.md) 中映射到本实现。

本参考文档记录每个 crate 的公开 API 表面。每个页面都会列出公开条目及其签名、对每条目功能的简短说明，以及源码中记录在该条目上的 `# Errors` / `# Panics` 行为。

## crate 布局

| Crate | 职责 |
|---|---|
| `cordis` | 门面（facade）。重导出 `cordis_core::*`、六个过程宏以及 `serde`/`schemars`。编写原生组件时你依赖的就是这个 crate。 |
| `cordis-core` | 全部运行时语义：context、effect、fiber、服务、事件、supervisor、原生组件与动态 host 桥。零 Wasmtime 依赖。 |
| `cordis-macros` | 六个过程宏（`service`、`event`、`component`、`component_impl`、`inject`、`apply`）。 |
| `cordis-guest` | 面向 WebAssembly 插件的 Guest SDK：生成的 kernel 绑定，加上你要实现的 `Guest` trait。 |
| `cordis-wasm` | 面向 Wasmtime 的 host 集成：engine、limits、factory、loader driver、HMR（热模块替换）manager 与 kernel 运行时。 |
| `cordis-loader` | 声明式 entry tree、config include 与事务性 reconcile。 |
| `cordis-cli` | `cordis` 命令：`check`、`run`、`inspect`、`build-component`。 |
| `cordis-timer` | effect 拥有的定时器（`timeout`、`interval`、`debounce`、`throttle`……），由 fiber 清理取消。 |
| `cordis-logger` | 带有限历史的结构化日志，以及 effect 拥有的 exporter。 |

## API 页面

- [context](context.zh.md) — `Context`：`root`、`fiber`、`extend`、`isolate`、`intercept`、`resolve_realm`、`intercept_layers`。
- [macros](macros.zh.md) — 六个过程宏与 ABI identity 模型。
- [effect](effect.zh.md) — effect 子系统：`EffectScope`、`EffectGuard`、`Disposer`、恰好一次（exactly-once）的 dispose 状态机、LIFO 恢复、`spawn_stream`。
- [fiber](fiber.zh.md) — `FiberMachine`、`FiberState`、`DesiredState`、转换机与惯性（inertia）规则。
- [fiber-machine](fiber-machine.zh.md) — 更深入的状态机不变量：Fibonacci 校验测试、转换合并、链式转换。
- [service](service.zh.md) — `ServiceId`、`ServiceKey`、`ServiceSpec`、`ServiceClient`、`ServiceDispatcher`、payload codec。
- [event](event.zh.md) — `EventId`、`EventSpec`、`EventMode`、`AsyncEvent`、`BailEvent`、`WaterfallEvent`、`EventTarget`、`Next`、`ControlFlow`。
- [native-component](native-component.zh.md) — 原生编写路径：`Component`、`ComponentContext`、`ComponentDefinition`、`NoDependencies`、`config_schema`。
- [dynamic](dynamic.zh.md) — 动态 host 桥：`ComponentFactory`、`ComponentInstance`、`InstanceHost`、`KernelHost`、`DynamicFiber`。
- [supervisor](supervisor.zh.md) — 单写者 actor：`Runtime`、`RuntimeHandle`、快照、命令表面、unload guard、环检测。
- [wasm](wasm.zh.md) — `cordis-wasm`：`WasmEngine`、`WasmLimits`、`WasmComponentFactory`、`ArtifactPolicy`、kernel WIT world。
- [config](config.zh.md) — `config.{json,yaml}` 文件语法：entry 字段、`isolate`/`intercept`、组件引用、经 JSON Schema 校验的 `config`、include 与 patch，以及 YAML 的 `!expr` Rhai 动态配置。
- [loader](loader.zh.md) — `cordis-loader`：`EntrySpec`、`EntryId`、`ComponentRef`、`EntryTree::reconcile`、`IncludeDocument`。
- [wasm-driver](wasm-driver.zh.md) — `cordis-wasm::loader`：`WasmApplication`、`WasmEntryDriver`、`BuiltinRegistry`、预检。
- [hmr](hmr.zh.md) — `cordis-wasm::hmr`：`HmrManager`、`HmrWatcher`、`ArtifactCache`、事务性回滚。
- [guest](guest.zh.md) — `cordis-guest`：生成的绑定、`KERNEL_ABI`、`encode`/`decode`、`export_plugin!`、`Guest` trait。
- [cli](cli.zh.md) — `cordis-cli` 子命令与 HMR run loop。
- [native-timer](native-timer.zh.md) — `cordis-timer`。
- [logging](logging.zh.md) — `cordis-logger`。

## 全文使用的约定

- **ABI identity** 是 `name` 加上对规范签名（canonical signature）计算的 32 字节 BLAKE3 哈希。原生组件与 WebAssembly 组件使用相同的 `ServiceId`/`EventId`，且只有当完整身份——名称**和**哈希——都匹配时，提供者才满足消费者的依赖。
- 源码中任何 **`#[doc(hidden)]`** 在此都会照此标注。它们是真实导出、但供宏生成的代码使用的 API，而非供直接使用。
- **错误**以 `CordisError`（core）或 crate 专属的错误枚举（`LoaderError`、`WasmHostError`、`HmrError`、`TimerError`）报告。
