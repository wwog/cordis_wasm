# 5. 配置与沙箱

插件从两个它从不拥有的输入决定自己做什么：`cordis.json` 中的 **config** 对象，以及 host 授予它的 **sandbox（沙箱）**。本章讲解 config 字节如何到达 `activate`、JSON Schema 强制什么，以及——对第 6 章最重要——`ArtifactPolicy`、`WasiCapabilities` 与 `WasmLimits` 如何把关 guest 实际被允许做什么。

## config 字节如何到达 `activate`

从 `cordis.json` 到你的 guest 的 `activate`，路径是一个序列化步骤外加一次校验停顿：

1. **loader 读取条目。** 在 `WasmEntryDriver::start_entry` 中，条目的 `config`（一个 `serde_json::Value`）与 factory 一起被交给 `mount_dynamic`。

2. **对照 schema 校验它。** `WasmApplication::reconcile` → `EntryTree::reconcile` → `validate_config`（loader.rs）让 config 通过*组件*声明的 schema：

   ```rust
   fn validate_config(entry: &ResolvedEntry, factory: &dyn ComponentFactory) -> Result<(), LoaderError> {
       let schema = serde_json::to_value(&factory.descriptor().config_schema).map_err(...)?;
       let validator = jsonschema::draft202012::new(&schema).map_err(...)?;
       if let Err(error) = validator.validate(&entry.spec.config) {
           return Err(LoaderError::InvalidConfig { entry: ..., path: ..., message: ... });
       }
       Ok(())
   }
   ```

   如果 config 不匹配，条目就无法*启动*——错误是响亮的，而在 `check` 中，坏 config 会在 preflight 阶段被捕获，根本不激活组件。

3. **config 被序列化成字节交给 guest。** 在 `WasmComponentInstance::activate` 中：
   ```rust
   let payload = serde_json::to_vec(&config)?;
   ```
   JSON 对象被转回原始字节，并作为 WIT `activate` export 的 `config: list<u8>` 参数传入。

4. **你的 guest 接收它们。** `activate(context: CallContext, config: Vec<u8>)`。config 正是第 3 步的那些字节。因此一个 config 条目 `{"port": 8080}` 会变成 JSON `{"port":8080}` 的字节，你的插件用 `serde_json::from_slice`（或任何你喜欢的方式）解析它们。guest SDK **不会**替你预解码 config——它把原始字节交给你。

关键洞见：config 在 **host 侧是一个 JSON 值**，在 **guest 侧是一个字节 blob**。JSON Schema 是 host 侧的门；guest 必须自己解码字节。

## `config_schema`

schema 是一个 JSON Schema（Draft 2020-12），以 descriptor 中的 JSON 字节提供。在 host 上，它在 `descriptor_from_wit` 中被解析并转成 `schemars::Schema`：

```rust
let config_value = serde_json::from_slice::<Value>(&descriptor.config_schema).map_err(...)?;
let config_schema = Schema::try_from(config_value).map_err(...)?;
```

所以一个*严格*的 schema 看起来像 counter 示例的：

```rust
config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
```

而一个接受真实 port/root 的 schema 看起来像：

```rust
config_schema: br#"
{
  "type": "object",
  "properties": {
    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
    "root": { "type": "string" }
  },
  "additionalProperties": false,
  "required": ["port"]
}"#.to_vec(),
```

再让一个 `cordis.json` 条目为：

```json
{ "id": "web", "component": "file:...", "config": { "port": 8080, "root": "./public" } }
```

这里 `port` 是**必需的**，`root` 是可选的；`additionalProperties: false` 拒绝 `"porr"` 之类的笔误。

两个值得注意的约定：

- **`additionalProperties: false` 是护栏。** 它把未知字段变成校验错误，而不是静默忽略。这是插件 config 的默认推荐——严格的 schema 在加载时抓住错误，而不是在运行时。
- **guest 自己解析字节。** 即使有 schema，你的 `activate` 也必须把原始字节变成类型化值。schema 保证 host *接受* config；它不会给 guest 一个已解码的值。

如果你想完全省略 `config_schema`，你是做不到的——WIT record 要求该字段。实践中"无 config"的惯用法就是上面那个严格的空对象 schema。

## `ArtifactPolicy` —— 能力门

host 的 `ArtifactPolicy`（runtime.rs）是 guest 可用的能力（capabilities）集合。

```rust
pub struct ArtifactPolicy {
    pub kernel_abi: String,
    pub allowed_capabilities: BTreeSet<Capability>,
    pub wasi: WasiCapabilities,
}

impl Default for ArtifactPolicy {
    fn default() -> Self {
        Self {
            kernel_abi: "0.1".to_owned(),
            allowed_capabilities: BTreeSet::new(),   // nothing allowed
            wasi: WasiCapabilities::deny_all(),
        }
    }
}
```

默认值拒绝**每一项**能力。因此一个想要网络访问的插件——第 6 章的整个要点——需要 host 构建一个在 `allowed_capabilities` 里带 `"network"` 的 `ArtifactPolicy`。该策略在 `WasmComponentFactory::from_bytes` 期间于三处被查阅：

1. **`validate_descriptor`** —— guest 在 descriptor 的 `capabilities` 列表中*声明*的每个能力必须出现在 `allowed_capabilities` 中，否则你会得到 `WasmHostError::CapabilityDenied`。
2. **`validate_wasi_imports`** —— 组件实际需要的每个 WASI import 必须既在 descriptor 声明的 `capabilities` *中*，也在 `allowed_capabilities` *中*。这堵住了 guest 在不声明的情况下针对 WASI 接口编译的漏洞。
3. **`ArtifactHash`**（hmr.rs）—— artifact 上的哈希包含该策略，因此一个需要与运行中策略不同的能力集合的 artifact 会得到不同的缓存键并被重新编译。

能力字符串是粗略的名称，不是细粒度的权限。`capability_for_wasi_import` 把一个 WASI 接口前缀映射到恰好一个能力：

| WASI import 前缀 | Capability |
|---|---|
| `wasi:io/`、`wasi:cli/`、`wasi:clocks/monotonic-clock` | *（无 —— 始终允许）* |
| `wasi:filesystem/` | `filesystem` |
| `wasi:sockets/` **或** `wasi:http/` | `network` |
| `wasi:random/` | `random` |
| `wasi:clocks/wall-clock` | `clock:wall` |
| 任何其他 `wasi:` 前缀 | 前缀本身 |

`wasi:sockets/` **和** `wasi:http/` 都映射到 `network`，这是对第 6 章至关重要的一个事实：使用 socket 或 HTTP 接口任一者的 guest 必须在它的 `capabilities` 中声明 `"network"`，host 也必须允许 `"network"`。

注意 CLI 总是用 `ArtifactPolicy::default()`——`crates/cordis-cli/src/main.rs` 把它传给 `check_entries` 与 `WasmApplication::new`。今天没有任何 CLI 标志能授予能力。所以一个有网络能力的 guest **无法**原样用随附的 CLI 运行；它需要一个构建了允许 `"network"` 的 `ArtifactPolicy` 的嵌入型 host，或一个把策略贯穿进去的 CLI 改动。第 6 章直白地陈述了这一点，并展示你需要的 guest 侧与 host 侧。

## `WasiCapabilities` —— WASI 沙箱

`ArtifactPolicy::wasi` 是一个 `WasiCapabilities`，它今天**只**关乎预打开的（preopened）文件系统目录（capability.rs）：

```rust
pub struct WasiPreopen {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

pub struct WasiCapabilities {
    preopens: Vec<WasiPreopen>,
}

impl WasiCapabilities {
    pub fn deny_all() -> Self { Self::default() }
    pub fn with_preopen(mut self, preopen: WasiPreopen) -> Self { ... }
}
```

默认的 `deny_all` 授予**零**个 preopen——连一个裸根都没有。想要读取自己目录的 guest 必须被显式授予一个 preopen，而该 preopen 必须满足两个约束：

- `guest_path` 必须是**相对**路径，且不得包含 `..`（由 capability.rs 的 `validate_guest_path` 校验）。意图是一个无法逃逸的沙箱化路径。
- `host_path` 在被传给 Wasmtime 之前会被 canonicalize。不存在的 host 路径是一个错误。

这刻意很粗粒度。没有 per-file 的读/写 ACL——preopen 在目录上是全有或全无。`WasiCapabilities::build` 把 preopens 变成一个 `WasiState`（一个 `WasiCtx` 加一个 `ResourceTable`），每个 guest store 都用它来构建。上面的 `wasi:` 能力名把关 guest 是否可*导入*该接口；preopens 把关它在导入之后有没有地方可去。

## `WasmLimits` —— 资源预算

Wasmtime 从 `WasmLimits`（lib.rs）强制每个 store 的资源 limits：

| 字段 | 默认值 | 它约束什么 |
|---|---|---|
| `fuel_per_call` | 10_000_000 | 每次 guest 调用在 `Trap::OutOfFuel` 之前消耗的 fuel。 |
| `epoch_deadline_ticks` | 1 | 在 `Trap::Interrupt` 之前的 epoch ticks。 |
| `max_memory_bytes` | 64 MiB | guest 线性内存增长上限。 |
| `max_table_elements` | 10_000 | table 元素。 |
| `max_instances` | 32 | core 实例。 |
| `max_tables` | 32 | table。 |
| `max_memories` | 32 | memory。 |
| `max_registrations` | 10_000 | host 每个 store 跟踪的注册数。 |
| `max_payload_bytes` | 1 MiB | 跨界传输的 service/event payload 最大体积。 |

其中两个值得点出，因为它们常绊倒人：

- **`fuel_per_call` + `epoch_deadline_ticks`** 在 `prepare_call`（lib.rs）中于**每次**调用时重新武装。它们是每调用预算，不是累积的——guest 中的死循环会以 `OutOfFuel` 或 `Interrupt` trap，但长时间运行的*正当* guest 不会跨调用被饿死。`docs/sundry/wasmtime-findings.md` 中的 wat findings 记录了这一点：你必须在每次调用前重置 fuel 与 epoch deadline，因为 Wasmtime 不继承前一次调用剩下的预算。
- **`max_payload_bytes`** 把关两个方向。host 在 `validate_payload` 中校验出站 payload，在 `validate_payload_limit` 中校验入站回复。超过它返回 `PayloadLimitExceeded`，以 `kernel-error` 的形式浮现给 guest。

这些 limits 是运行时的第二道防线（能力策略是第一道）：即使一个被*允许*导入 WASI 接口的 guest，也无法耗尽内存、泄漏 registrations、或永远自旋。

## 重新聚合 `capabilities` 字段

把它们拼起来。guest 的 descriptor 声明它*想要*什么：

```rust
capabilities: vec!["network".into()],
```

host 策略声明它*允许*什么：

```rust
let policy = ArtifactPolicy {
    kernel_abi: "0.1".into(),
    allowed_capabilities: BTreeSet::from([Capability::new("network")]),
    wasi: WasiCapabilities::deny_all(),
};
```

组件要能加载，**两边**必须对齐，且 WIT/WASI import 的分裂必须在两侧都被闭合：

- descriptor 的 `capabilities` 含 `"network"` → 通过 `validate_descriptor`；
- `"network"` 在 `allowed_capabilities` 中 → 通过 `validate_descriptor`；
- 组件的 `wasi:http`/`wasi:sockets` import 映射到 `"network"` → 通过 `validate_wasi_imports`。

任何一项失败，你都会在加载时得到 `CapabilityDenied` 错误。第 6 章走完整个"guest 想要 HTTP"的场景，包括为什么需要网络访问的插件恰恰需要这个策略，以及 CLI 当前的默认值在实践中意味着什么。

下一篇：[编写 web server 插件](06-writing-a-web-server-plugin.zh.md) —— 主要练习：一个提供 HTTP 服务的插件，以及真正的网络能力边界。
