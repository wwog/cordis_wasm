# 1. 第一次运行

在你写任何东西之前，先把随附的示例端到端地跑一遍。本章的目标是看清整条回路——构建 guest，然后 `check` 它们、`inspect` 它们、`run` 它们——并阅读把它们串起来的声明式配置。一旦回路变得熟悉，写新插件就多半只是照同样的形状填空。

从仓库根目录开始工作。两个 guest crate 是 `examples/wasm-counter-provider` 和 `examples/wasm-counter-consumer`。

## 构建 guest

```sh
cargo run -p xtask -- build-guests
```

这个命令以 `build-guests` 子命令调用 `xtask` 二进制。它按顺序做三件事：

1. 检查 `wasm32-wasip2` 是否已安装（`rustup target list --installed`）；若没有，它打印 `missing Rust target "wasm32-wasip2"; install it with rustup target add wasm32-wasip2` 并停止。
2. 运行 `cargo build --target wasm32-wasip2 -p wasm-counter-provider -p wasm-counter-consumer`。产物落在 `target/wasm32-wasip2/debug/wasm_counter_provider.wasm` 和 `target/wasm32-wasip2/debug/wasm_counter_consumer.wasm`——snake_case 名字就是把 crate 名中的连字符换成下划线。
3. 以指向该目录的 `CORDIS_GUEST_FIXTURES` 运行 host 集成测试。此处要紧的两个测试——`guest_sdk_artifacts_run_end_to_end` 和 `declarative_guest_artifacts_check_mount_route_and_shutdown`——把编译好的组件加载进真实 engine、激活它们、路由一次调用，然后干净地关停。若它们通过，就说明这些 artifact 足够好、可以运行。

在接近结尾处你应当看到：

```
running 2 tests
test runtime::tests::guest_sdk_artifacts_run_end_to_end ... ok
test loader::tests::declarative_guest_artifacts_check_mount_route_and_shutdown ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out
```

`target/wasm32-wasip2/debug/` 里的 `.wasm` 文件正是 config 按路径引用的东西。config 中没有东西加载 crate——它加载的是 *artifact*。

## 声明式配置

打开 `examples/wasm-app/cordis.json`：

```json
{
  "entries": [
    {
      "id": "consumer",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_consumer.wasm",
      "config": {},
      "isolate": {
        "example.counter": "example"
      }
    },
    {
      "id": "provider",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_provider.wasm",
      "config": {},
      "isolate": {
        "example.counter": "example"
      }
    }
  ]
}
```

有四个字段要紧：

- **`entries`** 是一个列表，即应用的插件树。每个对象是一个要挂载的插件。（是树——不只是列表——因为 entry 可以设 `group: true` 并携带 `children`；counter 示例保持扁平。）
- **`id`** 是 entry 在树中的稳定标识符。Reconciliation 以它为 key：在 `run` 监视期间编辑某个 entry 的 `config`，loader 会更新*那个* entry，而不是重启一切。
- **`component`** 是一个引用。两种形式是 `builtin:<name>` 和 `file:<path>`。`file:` 路径相对于包含 config 的目录解析（所以 `../../target/...` 是相对于 `examples/wasm-app/` 的）。`builtin:` 名字必须已经用 `BuiltinRegistry` 注册——随附示例都没用它，但嵌入的 host 正是靠它挂载进程内组件而无需 `.wasm` 文件。
- **`config`** 是一个 JSON 对象，在组件启动时传给它的 `activate` 函数。这里它是空的（`{}`），恰好是两个 guest 的 schema 都允许的——约束它的 schema 见第 5 章。

**`isolate`** 块是看起来不像普通 config 声明的那部分，而它处于核心地位。把它读作 `"service-name": "realm"`。这里 `"example.counter": "example"` 表示：*服务 `example.counter` 在这个 entry 内绑定到一个名为 `example` 的 realm。* provider 和 consumer 都声明了同样的映射，这正是它们落入同一 realm、从而互相满足的方式。

一个 `"realm"` 值可以是裸字符串（**global** realm，由每个冠以同一标签的 entry 共享），也可以是 `true`（**local** realm，只限定于该 entry 自身及其 children）。entry 上的 `isolate` map 表示"对于服务 `S`，把查找路由到 realm `R`"，覆盖父级原本会提供的东西。这正是让同一服务的两个 provider 共存的关键：把它们放进不同 realm，每个 consumer 解析其 context 所命名的那个 realm。

consumer 即便**消费**而非提供该服务，也照样通过 `isolate` 声明 `"example.counter"`。这是有意为之、而且要紧。`isolate` overlay 适用于 entry 的 descriptor 所 *touched*（触及）的任何服务——loader 收集 descriptor 全部 `inject` 和 `provide` 的并集，再对照 entry 的 map 逐个 isolate（见 `crates/cordis-wasm/src/loader.rs` 中的 `WasmEntryDriver::entry_context`）。所以 consumer 需要 realm 映射才能*找到* provider，而 provider 也需要它，因为 provider 把服务注册在一个 realm 里，只有该 realm 的 consumer 才能解析到它。

## Check——不激活而校验

```sh
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
```

期望输出：

```
ok: 2 entries, 2 components
  example.wasm-counter-consumer
  example.wasm-counter-provider
```

`check` **不**激活任何东西。它在 entry tree 上运行一个 preflight driver。对每个 entry，它解析 `file:` 路径、读取字节，并运行 `WasmComponentFactory::from_bytes`——它会编译组件、查询其 descriptor，并校验三件事：

- descriptor 的 `wit_version` 匹配 host 的 `kernel_abi`（`"0.1"`），否则你会得到 `KernelAbiMismatch`；
- `capabilities` 里的每个 capability 都在策略的 `allowed_capabilities` 中（默认：无），否则你会得到 `CapabilityDenied`；
- 组件需要的任何 WASI import 都既在 `capabilities` 中声明、又被策略允许——见 `crates/cordis-wasm/src/runtime.rs` 中的 `capability_for_wasi_import`，网络案例见第 6 章。

它还针对组件的 `config_schema` 校验每个 entry 的 `config`。你看到打印出的 descriptor `name` 就是那个名字。计数 `2 entries` 是 entry 的个数；`2 components` 是不同 descriptor 名的个数。

因为没有任何东西被激活，`check` 很廉价，正是"我是不是把路径或 schema 写错了"的恰当工具。counter 组件实现了 `Guest::call_service`、consumer 实现了 `handle_event`，但两者在 `check` 期间都不会运行。

## Inspect——真正挂载一切

```sh
cargo run -p cordis-cli -- inspect examples/wasm-app/cordis.json
```

期望输出（fiber id 是整数，可能不同）：

```
fibers: 3
  fiber=1 parent=None state=Pending dependencies=0
  fiber=2 parent=Some(FiberId(1)) state=Active dependencies=1
  fiber=3 parent=Some(FiberId(1)) state=Active dependencies=0
```

`inspect` 比 `check` 更进一步：它构造一个真正的 `WasmApplication`、reconcile entries，并调用 `settle`——它会等待运行时达到静默（quiescence）——然后打印 fiber tree。与 `check` 的差别在这里立刻显现：

- `fiber=1` 是 **root** fiber，由应用创建、从未被赋予组件。它处于 `Pending`，因为 root fiber 没有可激活的组件。
- `fiber=2` 和 `fiber=3` 是两个 entry。两者都是 `Active`，意思是它们的 `activate` 已运行并无错完成。

`dependencies` 列很有信息量。`fiber=2` 有 `dependencies=1`——那是 **consumer**，它注入 `example.counter`。`fiber=3` 有 `dependencies=0`——那是 **provider**，它只提供。`parent=Some(FiberId(1))` 说明两者都是 root 的 children。

这就是响应式模型在微观层面的"点睛之笔"：consumer 之所以达到 `Active`，是因为 provider 先行了一步。provider 不声明依赖，所以它立即激活；consumer 声明了对 `example.counter` 的必需 inject，所以它的 fiber 一直停在 `Pending`，直到有 provider 在它所解析的 realm 注册。那一刻，supervisor 通知 consumer，后者随之激活。`inspect` 给你看的是*已安顿*（settled）的状态，排序在那时已经发生。

## Run——活回路

```sh
cargo run -p cordis-cli -- run examples/wasm-app/cordis.json
```

然后是输出，接着一直等待，直到你按 Ctrl-C：

```
running 2 fibers across 2 artifacts; press Ctrl-C to stop
```

```
stopped 3 fibers
```

`run` 做 `inspect` 所做的一切，然后保持存活。具体来说：

1. 它构建应用、reconcile entries、等待静默——所以两个 guest fiber 在这里同样变成 `Active`。
2. 它向应用的 logger 注册 `ConsoleExporter`（`cordis-cli/src/main.rs` 调用 `application.driver().logger().register_exporter(...)`）。
3. 它打印 `running 2 fibers across 2 artifacts; press Ctrl-C to stop`。`2` 是 `snapshot.fibers.len() - 1`——*非 root* fiber 的数目。`2` 个 artifact 是当前为热重载而跟踪的不同 artifact 路径数。两个 guest 都是 `file:` 组件，所以两者都计入。
4. 它在那几条 artifact 路径**之外**再加上 config 路径，设置一个 `HmrWatcher`，然后进入 `tokio::select!` 循环，等待 Ctrl-C 和文件系统变更事件。

当你按 Ctrl-C，`run` 会关停应用（reconcile 到空树会按 child-first 顺序停止 entries，然后 retire root fiber），并打印 `stopped 3 fibers`——root 加上两个 guest。

试一次、让它待着，然后在它运行时于另一个终端编辑 `examples/wasm-app/cordis.json`：你应当看到树 reconcile 时打印 `config: committed 2 active fibers`，或者，若你把 config 弄到无效，则是一条 rollback 消息。那就是 config watch 路径在工作。重建某个 guest 的 `.wasm` 文件并 touch 它，会触发 artifact watch：`hmr: committed N entries`。

## 你现在所拥有的

你已经看过了锚定本教程其余一切的四个命令。带进下一章的关键心智模型：

- guest 是一个 **component artifact**（`.wasm`），而不是被链接进来的 crate。
- **config** 是一棵声明式的 entries 树；每个 entry 命名一个 component 引用、一份 config 和一组 realm 隔离。
- 组件的 **descriptor** 在*激活前*被读取，决定它运行需要什么、又提供什么。
- `check` 校验，`inspect` 挂载并安顿，`run` 挂载、安顿并监视。

下一步：[Anatomy of a guest plugin](02-plugin-anatomy.zh.md) — `Guest` trait、`PluginDescriptor`，以及 `activate` 如何把一个组件接入 kernel。
