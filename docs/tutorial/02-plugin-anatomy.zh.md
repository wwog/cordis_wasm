# 2. guest 插件剖析

guest 插件是一个实现单一 WIT world `cordis-plugin` 的 Wasmtime **组件**。`crates/cordis-guest/` 里的 SDK 把那个 world 变成少数几个由你填写的 Rust 条目。你在 guest crate 里写的每样东西，归根结底都是对单个 trait——生成绑定中的 `Guest` trait——的实现。

本章剖析随附的两个 guest。它们小到可以整篇通读，而且合起来覆盖了 trait 的每个方法，只除了那些读者要在第 6 章和第 7 章更有趣的插件里补上的部分。

## 生成的绑定

`crates/cordis-guest/src/lib.rs` 以一个宏调用开头：

```rust
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "cordis-plugin",
        pub_export_macro: true,
    });
}

pub use bindings::cordis::kernel::host;
pub use bindings::exports::cordis::kernel::plugin;
```

`wit_bindgen::generate!` 读取 `wit/kernel.wit` 并生成：

- `host` —— guest 可以调用的 **imports**。这些是 guest 用来够到运行时的函数：`call_service`、`provide_service`、`register_listener`、`dispatch_event`、`log`。
- `plugin` —— guest 必须实现的 **exports**。这就是带五个方法的 `Guest` trait，以及 `PluginDescriptor` record。

WIT kernel world（`crates/cordis-guest/wit/kernel.wit`）是 host 与 guest 之间固定的、带版本的契约。位于 `crates/cordis-wasm/wit/kernel.wit` 的 host 副本由 `crates/cordis-wasm/src/lib.rs` 里的一个测试断言与 guest 副本逐字节相同：

```rust
assert_eq!(
    include_str!("../../cordis-guest/wit/kernel.wit"),
    include_str!("../wit/kernel.wit")
);
```

这就是为什么恰好只有一个 kernel ABI：guest 与 host 针对同一份定义编译，任何不匹配都在加载时被捕获（第 8 章）。

## `Guest` trait

你要实现的生成 trait 位于 `plugin` 模块上。它的五个方法：

| 方法 | host 何时调用 | 它必须做什么 |
|---|---|---|
| `descriptor()` | 加载时、激活前 | 返回静态元数据：name、version、kernel WIT 版本、injects、provides、config schema、capabilities。 |
| `activate(context, config)` | 当所有被注入的服务都可用时 | 注册 services/listeners，启动后台工作。`config` 是 `cordis.json` 中该 entry 的 `config` 的字节。 |
| `deactivate(context)` | 当 fiber 卸载时 | 释放 `activate` 获取的、而运行时并不拥有的东西。 |
| `call_service(context, service, method, payload)` | 当另一个组件把一次调用路由到你所提供的服务时 | 在服务 `service` 上分发方法 `method`，带给定 payload；返回编码后的字节。 |
| `handle_event(context, event, listener_id, mode, payload, next_token)` | 当你监听的某个事件触发时 | 产生一个 `EventReply`。 |

你随处可见的两个导出 `ServiceId` 和 `EventId` 是那些 WIT record 的类型别名：

```wit
record service-id { name: string, abi-hash: list<u8> }
record event-id   { name: string, abi-hash: list<u8> }
```

`abi-hash` 是一个 32 字节 list。guest 用一个 `[u8; 32]` 常量——见示例里的 `COUNTER_ABI`——并在构造 `ServiceId` 时用 `.to_vec()` 转换它。host 在 `service_from_wit` / `event_from_wit` 中把 list 转回 `[u8; 32]`，并拒绝任何不是恰好 32 字节的长度。那个 hash 的含义在第 3 章解释；这里你只需知道它是一个必须由你提供的固定值。

## `descriptor()` 与 `PluginDescriptor`

`descriptor()` 是 host 在你组件做什么*一无所知*之前唯一会调用的方法，它返回的 `PluginDescriptor` 就是整个加载时契约。

```rust
fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        name: "example.wasm-counter-provider".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        wit_version: cordis_guest::KERNEL_ABI.into(),
        inject: Vec::new(),
        provide: vec![counter_service()],
        config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
        capabilities: Vec::new(),
    }
}
```

按 WIT record 声明的顺序，这些字段是：

- **`name`** —— `check` 输出和诊断中展示的人类可读身份。没有任何机制强制它在整个应用中唯一；它是元数据。两个示例用点号前缀（`example.wasm-counter-provider`）作为给名字加命名空间的约定。
- **`version`** —— semver 字符串。示例用 `env!("CARGO_PKG_VERSION")`，所以它跟随 crate 版本。
- **`wit_version`** —— 该 SDK 所针对的 kernel ABI。永远是 `cordis_guest::KERNEL_ABI`，即 `"0.1"`。host 把它与自己的 `ArtifactPolicy::kernel_abi`（也是 `"0.1"`）比较，并在激活前拒绝不匹配。
- **`inject`** —— 这个组件*需要*的服务。每一个都是完整的 `ServiceId`。正是这张表让 fiber 停在 `Pending`，直到每个 entry 都在所解析的 realm 里被提供。consumer 设 `inject: vec![counter_service()]`；provider 设 `inject: Vec::new()`。
- **`provide`** —— 这个组件*提供*的服务。除非你提供的服务按身份匹配，否则 host 不会把调用路由给你。provider 设 `provide: vec![counter_service()]`；consumer 设 `provide: Vec::new()`。
- **`config_schema`** —— 一份 JSON Schema，以 JSON 字节形式描述 `config` 的形状。host 解析它，并在*激活前*据此校验 entry 的 `config`。见下文。
- **`capabilities`** —— 这个组件需要许可才能使用的 WASI capabilities（例如 `"network"`、`"filesystem"`、`"random"`）。默认策略一个都不允许，所以对 counter 示例它是 `Vec::new()`。第 5 章和第 6 章会让它落到实处。

### 严格的 `config_schema` 意味着什么

provider 用：

```rust
config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
```

那份 JSON 解析成一份 JSON Schema（Draft 2020-12），它说的是：*config 必须是一个对象，并且不能包含 schema 未命名的任何属性。*由于这份 schema 也一个 `properties` 都没命名，唯一合法的 config 就是空对象 `{}`。这正是 `examples/wasm-app/cordis.json` 给两个 entry 都写 `"config": {}` 的原因，也是 `check` 接受它的原因。

这是运行时支持的最严格的"无配置"schema。如果你反而不提供 schema，或写成 `{}`（一个空对象，在 JSON Schema 里意为"任意值"），那么带任意字段的 `config` 也能通过校验。设计模式是：精确声明形状，并用 `additionalProperties: false` 来拒绝拼写错误。第 5 章展示了一份真正接受 `port` 和 `root` 的 schema，第 6 章用到它。

关于 `config_schema` 如何流过 host，有两个细节：

1. 这些字节必须既是合法 JSON *又是*合法 JSON Schema。host 在 `descriptor_from_wit`（runtime.rs）里用 `serde_json::from_slice::<Value>` 解析它，然后用 `Schema::try_from` 把它转成 `schemars::Schema`。任一步失败，你都会在加载时得到 `Descriptor` 错误。
2. 它由**两条**路径校验。`check` 在 preflight 期间校验 config 是否符合 schema，而 `run`/`inspect` 在真正的 `WasmEntryDriver` 启动 entry 时再次校验（见 `crates/cordis-wasm/src/loader.rs` 中的 `validate_config`）。两者都用同一个 `jsonschema::draft202012` validator 作用于 JSON 序列化后的 `config`。

## `activate`、`deactivate` 与 thread-local 注册模式

provider 的 `activate` 是 guest 的心脏：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::provide_service(context, &counter_service())?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}

fn deactivate(_context: CallContext) -> Result<(), KernelError> {
    REGISTRATION.with(|slot| slot.borrow_mut().take());
    Ok(())
}
```

有两件事值得注意。

首先，`host::provide_service` 返回一个 `Registration`——指向 host 侧注册的句柄。guest SDK 目前靠手工穿线：示例把它存在一个 `thread_local!` 槽里，好让 `deactivate` 能取回它。它还仰赖 host 对清理拥有最终权威（README 的"guest 不可信"假设）：即便 guest 从不 drop 那个句柄，host 也会在 fiber 卸载时清掉该 store 的全部注册。第 3 章把这个手工穿线与把所有细节都藏起来的原生宏路径相对照。

其次，`activate` 是 guest 注册任何东西的唯一地方。不存在"每次调用都注册"——一个服务在激活期间注册一次，然后一直待到 deactivation。这正是 `deactivate` 必须撤销它的原因：运行时既然在卸载时自动清理注册、不会再调用一次 `deactivate` 来释放它，那么获取（acquisition）的两头都要 guest 自己负责。

## `call_service` —— 提供一个方法

provider 的 `call_service` 分发一个方法：

```rust
fn call_service(
    _context: CallContext,
    service: ServiceId,
    method: u32,
    payload: Vec<u8>,
) -> Result<Vec<u8>, KernelError> {
    if service.name != counter_service().name || method != GET_METHOD {
        return Err(KernelError::InvalidArgument("unknown service method".into()));
    }
    let increment = if payload.is_empty() {
        1
    } else {
        cordis_guest::decode::<u64>(&payload)?
    };
    let value = VALUE.with(|value| {
        let mut value = value.borrow_mut();
        *value += increment;
        *value
    });
    cordis_guest::encode(&value)
}
```

一个 `call_service` 实现的形状总是相同的：

- **匹配服务和方法。**这里 guest 同时检查服务名和一个数值 `method` 常量（`GET_METHOD = 1`）。调用不匹配就返回错误。`method` 是 `u32`，因为 kernel 边界是类型擦除的；这个数值 id 是方法签名 BLAKE3 hash 的头四个字节（第 3 章）。
- **解码 payload。**payload 是 `MessagePack` 字节。`cordis_guest::decode::<T>(&bytes)` 把它变成 `T`，`cordis_guest::encode(&value)` 把回复编回去。空 payload 的情形——`payload.is_empty()`——是个小便利：consumer 用 `&1_u64` 调用（编码成几个字节），所以在这里不会命中，但 guest 容忍空 body、默认取 `1`。
- **返回编码字节。**

注意这里 guest 在 `call_service` 里从不检查 `CallContext`。它被穿线而过，是因为 host 会校验该 context 属于这个 store，而发出*子调用*——比如调用 `call_service` 的 consumer——的 guest 需要它。provider 只是忽略它。

## `handle_event` —— 真实示例直接透传

provider 的 `handle_event` 本质上是个 no-op：

```rust
fn handle_event(
    _context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    Ok(EventReply::ContinueValue(payload))
}
```

它把 payload 原样回显成一个 `ContinueValue`。这是 guest 收到事件时必须返回的最小量；它并不真的*监听*任何东西（它从不调用 `register_listener`）。第 4 章解释五种 mode 以及 `ContinueValue` 对比 `BreakValue` 的含义，第 7 章把它变成一个真正的 listener。

## `export_plugin!`

每个 guest crate 的最后一行是宏调用：

```rust
cordis_guest::export_plugin!(CounterProvider);
```

它展开成：

```rust
bindings::export!(CounterProvider with_types_in bindings);
```

那是 `wit_bindgen` 的 export 宏，它把你的 `Guest` impl 接到生成的组件 exports 上，好让 Wasmtime 能调进来。它必须在 crate 根部、以实现 `Guest` 的类型命名，且只调用一次。如果你忘了，组件仍能编译但什么也不导出，host 将无法链接它。

## `CallContext`

每个 host 入口点都收到一个 `CallContext`：

```wit
record call-context {
    fiber-id: u64,
    effect-id: u64,
}
```

两个字段是*当前执行中* fiber 访问作用域的身份：拥有这次调用的 fiber，以及它正运行其内的 effect（fiber 的一次激活）。host 用它做两件事：

- **校验。**在每个 `host::*` 入口点，`crates/cordis-wasm/src/runtime.rs` 都会调用 `validate_context`，把 context 的 `fiber_id` 和 `effect_id` 与 store 挂载时的相比。不匹配就返回 `InvalidArgument`——host 正是靠这个拒绝把 context 从一个 fiber 偷渡到另一个的 guest。
- **子调用。**当 guest 回调运行时（比方说 `call_service`），它传入自己被给予的 context，host 用该 context 解析*当前* fiber 的 committed dependency view。这就是为什么 guest 必须把 context 穿线而过而不是自己重建：关于一个 fiber 能看见什么，host 是唯一权威。

永不回调 host 的 guest 可以忽略 context（counter provider 在 `call_service` 里就是这样）；要回调的则必须把它原样传下去。

## 组装起来的零件

因此一个 guest crate 永远是同一个骨架。填上四个数据方法和一个或多个 dispatch/event 方法，然后导出：

```rust
use cordis_guest::host::{CallContext, EventId, EventMode, EventReply, KernelError, ServiceId};
use cordis_guest::plugin::{Guest, PluginDescriptor};

struct MyPlugin;

impl Guest for MyPlugin {
    fn descriptor() -> PluginDescriptor { /* name, version, wit_version, inject, provide, config_schema, capabilities */ }
    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> { /* register services/listeners */ }
    fn deactivate(_context: CallContext) -> Result<(), KernelError> { /* release them */ }
    fn call_service(_context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>) -> Result<Vec<u8>, KernelError> { /* dispatch */ }
    fn handle_event(_context: CallContext, event: EventId, listener_id: u64, mode: EventMode, payload: Vec<u8>, next_token: Option<u64>) -> Result<EventReply, KernelError> { /* reply */ }
}

cordis_guest::export_plugin!(MyPlugin);
```

`activate`/`deactivate` 这一对和 descriptor 是组件通过加载的最低要求；`call_service` 和 `handle_event` 是行为所在之处。下一章展示 descriptor 的 `provide`/`inject` 两半如何连接两个组件，以及 guest 如何手工穿线 kernel 边界。

下一步：[Services and injection](03-services-and-inject.zh.md) — `provide`/`inject`、`ServiceId` 与 ABI hash、realms，以及与原生宏的对照。
