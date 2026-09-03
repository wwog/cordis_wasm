# 4. 事件

服务是直接调用：消费者指名一个提供者并向它要东西。**事件**是公告的那一半——一种说"发生了某事"而不必知道哪些插件（如果有）在监听的方式。counter 示例只*提供*一项服务；本章构建 kernel 的事件那一半，以便第 7 章把它变成真正呈现数据的监听器。

## WIT 事件面

guest 通过三个 host import 通信事件，定义于 `crates/cordis-guest/wit/kernel.wit`：

```wit
record event-id {
    name: string,
    abi-hash: list<u8>,
}

enum event-mode {
    emit,
    parallel,
    serial,
    bail,
    waterfall,
}

variant event-reply {
    continue-value(list<u8>),
    break-value(list<u8>),
}

register-listener: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
) -> result<registration, kernel-error>;

dispatch-event: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
    payload: list<u8>,
    next-token: option<u64>,
) -> result<event-reply, kernel-error>;
```

`event-id` 在结构上与 `service-id` 相同——一个 name 加一个 32 字节 ABI 哈希。这个哈希服务于与 services 相同的目的：它钉住事件的 *payload 契约*，因此即使组件是独立编译的，监听器与发出方（emitter）也会在线上格式上达成一致。

guest 还实现 `plugin` export 的事件那一半：

```wit
handle-event: func(
    context: call-context,
    event: event-id,
    listener-id: u64,
    mode: event-mode,
    payload: list<u8>,
    next-token: option<u64>,
) -> result<event-reply, kernel-error>;
```

## 五种 `EventMode`

WIT 与 native core 一致地同意五种模式。native 宏把每个名字映射到 `crates/cordis-macros/src/lib.rs` 中的一个 core runtime 类型：

| WIT 模式 | Native core 类型 | Dispatch 语义 |
|---|---|---|
| `emit` | `AsyncEvent`（fire-and-forget） | 广播给每个匹配的监听器，不 await，不收集结果。 |
| `parallel` | `AsyncEvent` | 所有匹配的监听器并发运行；所有结果被 await 并收集。 |
| `serial` | `AsyncEvent` | 监听器按顺序运行；第一个 `Break` 获胜并停下其余部分。 |
| `bail` | `BailEvent` | serial 的同步版本。 |
| `waterfall` | `WaterfallEvent` | 洋葱中间件：每个监听器得到一个 `next()` continuation。 |

native 事件类型（`AsyncEvent`、`BailEvent`、`WaterfallEvent`，位于 `crates/cordis-core/src/event.rs`）实现*运行时*语义：`AsyncEvent::parallel` 并发 join 所有监听器，`AsyncEvent::serial` 在第一个 `Break` 处停下，`BailEvent` 是同步等价物，`WaterfallEvent` 把一次性 `Next` continuation 穿过整条链。每个都返回一个 `ControlFlow<B>`，其中 `Continue` 表示"继续"，`Break` 表示"停下，这就是答案"。

### guest 目前能看到什么

现在说说实在话。**host kernel** 中面向 guest 的 dispatch 路径（`RuntimeKernel::dispatch_event`，`crates/cordis-wasm/src/loader.rs`）**尚未**把一个事件扇出到多个监听器，也**不**编排 waterfall 的 `next()` 链。它只路由单个调用：

```rust
fn dispatch_event(&self, _: FiberId, call: EventCall) -> ComponentFuture<'_, EventReply> {
    Box::pin(async move {
        let owner = self
            .listeners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(call.event.clone(), call.listener_id))
            .copied()
            .ok_or_else(|| CordisError::ComponentFailed {
                component: call.event.to_string(),
                message: format!("listener {} is not registered", call.listener_id),
            })?;
        self.route(owner).await?.call_event(call).await
    })
}
```

监听器映射是 `BTreeMap<(EventId, u64), FiberId>`——它把一个**特定的** `listener_id` 键到单个所属 fiber。一次 dispatch 找到那一个 fiber，并把一次 `handle_event` 调用路由给它。`mode` 字段经由 `EventCall` 携带给 guest 看，`next_token` 也被透传，但 host 不枚举监听器、不并发运行它们、不聚合 `Break`，也不管理 `next()` token 链。

这意味着：**五种模式是声明的契约，native core 完整实现它们，但单个 Wasm guest 今天每次只收到一个事件调用，并被期望自己解读 `mode` 与 `next_token`。** 第 7 章的 status/tracking 插件正是利用这一事实——它注册单个监听器，并在 `handle_event` 中对该事件作出反应。如果你需要跨多个 Wasm guest 的真正 fan-out 或 waterfall 链，host kernel 需要 native `event.rs` 已经具备的、识别模式（mode-aware）的 dispatch 循环；那是你要扩展的边界。

对插件作者的实际后果：为你想反应的每样东西注册一个监听器，自己决定回复，不要依赖 host 去编排任何顺序。

## `EventId { name, abi_hash }`

与 `ServiceId` 形状相同。guest 用 name 和 32 字节常量手工构造它：

```rust
const STATUS_ABI: [u8; 32] = [0x51; 32];

fn status_event() -> EventId {
    EventId {
        name: "example.status".into(),
        abi_hash: STATUS_ABI.to_vec(),
    }
}
```

与 services 完全一样，host 在 `event_from_wit` 中把进来的 `list<u8>` 转成 `[u8; 32]`，并拒绝任何其他长度：

```rust
fn event_from_wit(event: wit::EventId) -> Result<cordis_core::EventId, wit::KernelError> {
    let hash = <[u8; 32]>::try_from(event.abi_hash.as_slice()).map_err(|_| {
        wit::KernelError::InvalidArgument("event ABI hash must contain 32 bytes".to_owned())
    })?;
    Ok(cordis_core::EventId::new(event.name, hash))
}
```

因为 `EventId` derive `Ord`，一对在 name 与 hash 上达成一致的独立编写组件就是*同一个*事件。单凭 name 会让两个无关的事件碰撞。

## 注册一个监听器

监听器从 `activate` 注册，通过调用 host import：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(context, &status_event(), LISTENER_ID, EventMode::Serial)?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

`register_listener` 接收四个参数：context、event id、一个 **`listener_id`**（`u64`）以及 mode。`listener_id` 由你选择——它是 host 映射中标识*这个*监听器的稳定键。在 host 上它变成 `RegistrationRequest::Listen { event, listener_id, mode }`，落进以 `(event, listener_id)` 为键的 kernel 监听器表。

返回的 `Registration` 是一个 effect-owning handle，与 `provide_service` 完全一样。把它存起来（示例用 `thread_local!`），使 `deactivate` 能释放它，并在 fiber 卸载时把 host 当作清理的权威。

注册时两条 host 侧规则值得注意：

- **`(event, listener_id)` 对必须唯一。** `crates/cordis-wasm/src/loader.rs` 中的 `RuntimeKernel::register_listener` 拒绝重复键：
  ```rust
  if listeners.contains_key(&key) {
      return Err(... "listener {listener_id} is already registered");
  }
  ```
  在同一个 fiber 里，对每个不同的事件反应注册不同的 `listener_id`。
- **只有注册之后 dispatch 才有效。** 对未注册 listener id 的 `dispatch_event` 返回 `listener {id} is not registered`。这与 services 相呼应：在 descriptor 中声明兴趣还不够，你必须在 `activate` 中实际调用 `register_listener`。

## 发出一个事件

发出事件是 host import `dispatch_event`。guest 调用它向它认识的监听器投递一个 payload：

```rust
let reply = host::dispatch_event(
    context,
    &status_event(),
    LISTENER_ID,
    EventMode::Serial,
    &payload,
    None,           // next_token
)?;
```

在 host 上，`GuestState::dispatch_event` 校验 context 与 payload，把 WIT event 转成 core 的 `EventId`，把所有东西包进一个 `EventCall`，并路由到所属 fiber。它返回的 `EventReply` 被转回 WIT（`Continue(payload)` → `ContinueValue(payload)`、`Break(payload)` → `BreakValue(payload)`）。

注意发出方传入一个 `listener_id`——你向一个*特定的*监听器发出，而不是"所有关注该事件的人"。这是上面讨论的单监听器 dispatch 边界的推论。在 native 路径中你会向一个 `EventTarget`（global 或 per-realm）发出，运行时解析所有匹配的监听器；在今天的 guest 路径中，你指名你想到达的那一个监听器。

## `EventReply` —— 监听器返回什么

`handle_event` 返回一个 `EventReply`：

```rust
fn handle_event(
    _context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    // react to the payload ...
    Ok(EventReply::ContinueValue(output_bytes))
}
```

两个变体镜像 `ControlFlow`：

- **`ContinueValue(bytes)`** —— "继续 / 没有决定性内容要报告。" 纯观察者返回它。在 serial 或 waterfall 链中它表示该监听器没有短路。
- **`BreakValue(bytes)`** —— "这是答案，停下。" 在 serial 或 bail 链中它打破循环并成为结果。在 waterfall 中，不调用 `next()` 就返回就是短路——guest 表面把它建模为 `BreakValue`。

因为 host 今天对每次 dispatch 只路由一个监听器，这两个变体的*有效*用法是：provider/dispatcher 把 `BreakValue` 读作"监听器有话要说"，把 `ContinueValue` 读作"没有新消息"，插件据此决定接下来做什么。第 7 章用 `ContinueValue` 做一个日志观察者，并展示 `BreakValue` 会在哪里携带一个决策。

## 一个小型 provider+listener 示例

综合起来，一个发出事件的 provider 与一个监听的 consumer 看起来如下。**provider** 不声明任何服务，并在 `activate` 期间发出：

```rust
// provider (illustrative shapes — real code uses the same calls as the counter provider)
fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> {
    let payload = cordis_guest::encode(&"started")?;
    let _ = host::dispatch_event(context, &status_event(), LISTENER_ID, EventMode::Serial, &payload, None)?;
    Ok(())
}
```

**consumer** 不注入任何服务（它不是服务），不声明任何服务，并在 `activate` 中注册一个监听器：

```rust
// consumer
fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        name: "example.wasm-status-consumer".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        wit_version: cordis_guest::KERNEL_ABI.into(),
        inject: Vec::new(),
        provide: Vec::new(),
        config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
        capabilities: Vec::new(),
    }
}

fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(context, &status_event(), LISTENER_ID, EventMode::Serial)?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}

fn handle_event(
    context: CallContext,
    event: EventId,
    listener_id: u64,
    mode: EventMode,
    payload: Vec<u8>,
    next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

这是第 7 章构建成真正 status-tracking 表面的形状。与 counter consumer 的关键区别是它**注册监听器**而非调用服务——事件是响应式的（reactive），服务是命令式的（imperative）。

## 事件与服务的对比，一张表

| | 服务 | 事件 |
|---|---|---|
| 注册 | 在 `activate` 中调用 `host::provide_service` | 在 `activate` 中调用 `host::register_listener` |
| 触发 | 消费者 `call_service` | 发出方 `dispatch_event` |
| 目标 | 经由调用方的 committed view + realm 解析 | 一个特定的 `(event, listener_id)` |
| Guest 方法 | `call_service` | `handle_event` |
| 返回 | `Vec<u8>`（编码后的回复） | `EventReply`（`ContinueValue` / `BreakValue`） |
| 用例 | "现在给我一个值" | "发生了某事；作出反应" |

下一篇：[配置与沙箱](05-config-and-capabilities.zh.md) —— config 字节、JSON Schema、`ArtifactPolicy`、`WasiCapabilities` 与 `WasmLimits`。
