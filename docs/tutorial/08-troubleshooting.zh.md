# 8. 故障排查

这个运行时对失败毫不含糊——它宁可加载时就响亮地失败，也不愿默默拖到一个令人困惑的运行时错误。本章列出你最可能撞上的失败模式、每种模式的实际含义，以及如何读懂它。其中大多数会在任何组件激活之前就被 `cordis check` 检查出来，只有少数只在 `run` 中浮现。

## PENDING / 永不激活：某个 `inject` 缺少 provider

最常见的情形是某个 fiber 停在 `Pending`，看起来什么都不做。

**症状。** `cordis inspect` 显示一个 `state=Pending` 的 fiber 永远变不成 `Active`。完全没有错误消息——fiber 就是不肯往前走。

**原因。** 一个必需的 `inject` 在 consumer 解析所在的 realm 里没有 provider。descriptor 可以把该声明的服务都声明对，但 *realm 的接线*错了。回想第 3 章：路由是调用方 context 的函数，而 `isolate` 映射决定某个服务在哪个 realm 中解析。consumer 会一直停在 `Pending`，直到有 provider 在那个 realm 里注册。

**最常见的错误原因。** consumer 与 provider 对同一个服务声明了*不同*的 `isolate` 映射——比如 consumer 有 `"example.counter": "example"`，而 provider 什么都没有，或者用了不同的 label。它们永远不会相遇。检查 `cordis.json` 里的两个 entry，是否对同一个服务名使用了同一个 realm label。这正是 counter example 在*两个* entry 上都重复这份映射的原因。

**其他原因。** provider 从未在 `activate` 里调用 `provide_service`（所以它是候选者，但现场并没有 provider）。或者 provider 的 fiber 在加载之后立刻 `Failed`——这种情况下 consumer 看不到任何 provider，只能停在 `Pending`。在责怪 consumer 之前，先看看 provider 自己的状态。

**如何检查。** 对比 `isolate` 块，并确认 provider 是 `Active`。一个永不解析的必需 inject，按设计就是一个无声的 `Pending`——运行时在等待，而不是在失败。论文把这个情形称为"依赖环让相关组件永久失活……仅凭依赖声明即可预测"；参见 [semantics.md](../semantics.zh.md)。

## `KernelAbiMismatch`

**症状。** 一个 `Descriptor` 或 `Driver` 错误，内容包含 `kernel ABI mismatch: expected 0.1, got X`。

**原因。** guest 的 `PluginDescriptor::wit_version` 不等于 host 的 `ArtifactPolicy::kernel_abi`。host 要求 `"0.1"`（来自 `ArtifactPolicy::default`），guest 则必须把 `wit_version` 设为 `cordis_guest::KERNEL_ABI`——也就是 `"0.1"`。如果你复制了一个旧的 descriptor，或硬编码了别的字符串，不匹配就会在加载时出现。

**修复。** 把 `wit_version` 设为 `cordis_guest::KERNEL_ABI.into()`。检查位于 `validate_descriptor`（runtime.rs）：

```rust
if descriptor.kernel_abi.as_ref() != policy.kernel_abi {
    return Err(WasmHostError::KernelAbiMismatch { expected: ..., actual: ... });
}
```

这是"两份 WIT 拷贝必须一致"这条规则的安全阀。如果 kernel WIT 有朝一日发生变化，guest SDK 与 host 会一起提升 `KERNEL_ABI`，旧的 artifact 会被拒绝，而不是被默默带进一场错误的通信。

## `CapabilityDenied`

**症状。** `cordis check`（或 `run`）以 `component capability \`network\` is denied` 或 `WASI import ... requires undeclared capability network` 失败。

**原因。** 两套彼此独立的检查，都在 `validate_descriptor` / `validate_wasi_imports` 中：

- **声明了的** capability 不在 `ArtifactPolicy::allowed_capabilities` 里。你的 descriptor 列出了 `"network"`，但策略（CLI 的默认值，什么都不允许）没有。
- **导入了的** WASI 接口需要 descriptor 没声明的 capability。一个导入了 `wasi:sockets/` 或 `wasi:http/`、却把 `"network"` 从 `capabilities` 列表里漏掉的 guest，即使策略*本来会*放行 `network`，也会从 `validate_wasi_imports` 得到这个错误。

第一种是"你请求了 host 不会授予的权限。"第二种是"你用了没声明的东西。"两者都会大声失败，而且都发生在加载时、组件激活之前——这正是要点。一个具备网络能力的 guest 究竟需要什么，完整说明见第 6 章。修复需要 host 构造一个授予该 capability 的 `ArtifactPolicy`；CLI 今天并不提供这个入口。

## `InvalidConfig` / schema 错误

**症状。** `entry <id> configuration is invalid at <path>: <message>`，或来自 `check` 的、提到 schema 的 `Driver` 错误。

**原因。** entry 的 `config` 与组件的 `config_schema` 不匹配。错误里的具体路径会告诉你问题在哪个字段。第 5 章 `additionalProperties: false` 这道护栏是常见的触发点——一个没料到的 key，或某个已声明属性用了错误的类型。

**修复。** 要么把 config 改正去匹配 schema；要么，如果插件确实需要该字段，就更新 `config_schema` 去声明它。注意，schema 是*组件自己的*声明：一个想把 `port` 当作整数的插件必须声明它，否则 host 会把整数 config 当作违反 schema 而拒绝。

**一个相关的陷阱。** 一份 config 是合法 JSON，但在 *descriptor*（`config_schema`）里不是合法的 JSON Schema——这会被单独作为一个 `Descriptor` 错误捕获：host 用 `serde_json` 解析它，然后再 `Schema::try_from`。所以一个格式错误的 `config_schema`（而不只是格式错误的 `config`）同样是加载时失败。这些 bytes 必须既是合法 JSON，*又*是合法的 Draft 2020-12 schema。

## 缺少 `wasm32-wasip2` target

**症状。** `cargo build --target wasm32-wasip2 -p ...` 以一个关于未知 target 的错误失败，或 `xtask build-guests` 打印 `missing Rust target "wasm32-wasip2"; install it with rustup target add wasm32-wasip2`。

**原因。** guest 组件编译到 WASIp2，而不是更旧的 `wasm32-wasi`。如果没安装该 target，就什么都构建不了。

**修复。**

```sh
rustup target add wasm32-wasip2
```

然后用 `rustup target list --installed` 验证——应当能看到 `wasm32-wasip2` 那一行。

## `cordis check` vs `cordis run`——两者的区别

这两条命令共享大部分路径，但在一个关键点上不同，而这个不同正是常见的困惑来源：

|  | `check` | `run`（和 `inspect`） |
|---|---|---|
| 读取 config 并解析 entries | 是 | 是 |
| 编译每个组件、读取 descriptors | 是 | 是 |
| 对照 schema 校验 config | 是 | 是 |
| 检查 ABI 与 capability 策略 | 是 | 是 |
| **激活组件** | **否** | **是** |
| 注册日志 exporter | 否 | 是 |
| 监听 HMR | 否 | 是 |

实际后果：`check` 可能成功而 `run` 失败，因为 `check` 从不运行 `activate`。一个在 `check` 下正常激活、却在 `activate` 里失败（decode 错误、`provide_service` 返回错误、panic）的 guest，会通过 `check` 却在 `run` 中失败。反过来，`check` 是那个快速的"我的声明接线对了吗"工具，省去了挂载一切的开销。

两条命令另一处分岔：`check` 使用一个 `PreflightDriver`，它在 `stop` 时什么都不做就停手；而 `run`/`inspect` 会挂载真正的 `DynamicFiber`。所以只会在激活之后出现的 fiber 级生命周期错误，对 `check` 是不可见的。

按这个顺序诊断：先跑 `check`（声明 + 策略 + schema），再 `inspect`（它真的挂载并安定下来了吗？fiber 都处于什么状态？），然后 `run`（activate + 日志 + 监听）。

## HMR 监听 config vs artifact

`cordis run` **同时**监听 config 文件和应用当前正在运行的每一个 `file:` artifact。两条监听路径的行为不同：

- **Config 变化** → `application.reconcile(entries)`。config 被重新加载并做 diff；有效的变更会提交（`config: committed N active fibers`），无效的会整体回滚（`config: transaction rolled back: <error>`）。失败时整棵先前的树都会被保留。
- **Artifact 变化** → `application.driver().reload_paths(paths)`。被改动的 `.wasm` 会被重新编译，受影响的 fiber 被事务性地替换：成功时是 `hmr: committed N entries`，失败时是 `hmr: transaction rolled back: [<entries>]`。

**"我编辑了却什么都没发生"的症状。** 你改了某个 config 值、重建了 guest、或碰了某个 `.wasm`，但正在运行的应用没有反应。检查磁盘上到底哪个文件变了，以及它是否在被监听的集合里。watcher 追踪的是规范的 artifact 路径（`artifact_paths()`）加上 config 路径。如果你把产物构建到了与 config 所指向*不同*的路径，或编辑的是一个不会重新编译到 artifact 路径的源文件，那就什么都不会变——watcher 以 artifact bytes 为 key，而不是以源文件为 key。重建 artifact（`cargo build --target wasm32-wasip2 -p ...`），并确保 config 的 `file:` 路径指向重建后的产物。

另外注意：`check` 完全没有监听路径。只有 `run` 会监听。

## fiber 在激活期间 `Failed`

**症状。** `inspect` 显示一个处于 `Failed` 状态的 fiber，或 `run`/`inspect` 在启动时报告组件错误。消息常常是一个包裹着 guest 错误的 `ComponentFailed`。

**原因。** `activate` 返回了错误，或 guest 内部的 panic 被捕获了。第 2 章的"让 `activate` 抛异常"就是大声的版本。常见的 guest 侧原因：对 config 的 `decode` 失败（config bytes 与 guest 的预期对不上）、`provide_service` / `register_listener` 返回 kernel 错误（例如因注册上限而来的 `CapabilityDenied`，或重复的 listener id）。

**修复。** 让 `activate` 返回一个有意义的 `KernelError`，而不是 panic。运行时把 guest panic 包装成 `component panic while polling guest call`，并把 fiber 标为 `Failed`；返回的错误则以 kernel error 的形式浮现出来。比起 panic，优先返回错误，这样消息才是真正的成因。

## config 的 `file:` 路径错了

**症状。** `check` 或 `run` 解析 entry 时出现 `InvalidComponentRef`，或一条"no such file"式的 `Driver` 错误。

**原因。** `component` 值不是合法的 `file:` 或 `builtin:` 引用，或者路径无法相对 config 所在的目录解析。`ComponentRef::parse` 要求 `file:` 前缀和一段非空路径；路径会被拼到 config 的父目录下。

**修复。** 使用 `file:`（用于相对 config 的路径）或 `builtin:<name>`（用于已注册的 builtin）。确认路径是相对包含 config 的那个文件解析的，而不是相对 CWD。unknown-scheme 错误是一道防拼写的护栏：没有 `file:` 前缀的裸路径会被拒绝。

## 接下来看哪里

- API 参考覆盖了这些现象背后每一个的确切签名：[guest](../api/index.zh.md) 讲 `Guest` trait，[service](../api/index.zh.md) 讲 `ServiceId`/`InjectSpec`，[event](../api/index.zh.md) 讲 `EventMode`/`EventReply`，[wasm](../api/index.zh.md) 讲 `ArtifactPolicy`/`WasmLimits`。
- 每个行为背后的语义模型在 [semantics.md](../semantics.zh.md)——尤其是解释 PENDING 情形的"组件只在依赖齐备时才激活"规则（定理 70），以及事务性 reconcile / HMR 对照表。
- 完整的错误枚举：`LoaderError` 在 `crates/cordis-loader/src/entry.rs`，`WasmHostError` 在 `crates/cordis-wasm/src/lib.rs`，`CordisError` 在 `crates/cordis-core/src/error.rs`。

这是最后一章。现在你已经掌握工具：能读一份 `cordis.json`，能编写一个提供服务、监听事件的 guest，也能诊断插件为何无法启动。第 6、7 章那条诚实的边界——没有 `wasi:http`、没有 UI——不是你理解上的缺口，而是运行时当前的边缘。当你去扩展它时，从 host 的 `ArtifactPolicy` 和 guest 的 `capabilities` 列表入手，其余的自然会跟着来。
