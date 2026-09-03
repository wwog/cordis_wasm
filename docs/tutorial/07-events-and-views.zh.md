# 7. 事件与视图

第二个主要练习：一个插件，当某个事件触发时作出反应，并把结果呈现到你能观察到的地方。这里正是需要给 "view" 一个诚实定义的地方，因为这个仓库没有 DOM、没有 UI 框架、没有屏幕。

## 这里的 "view" 指什么

`cordis-wasm` 中没有图形层，也没有把插件的输出渲染给用户这回事。这个运行时里的 "view"，是一个**事件触发时你可以写入的可观察 surface**。你有四个现实可行的去处，按它们今天工作的直接程度排序：

1. **日志，经由 `host::log`。** host import `log(context, level, message)` 写入应用的 `Logger`。在 `cordis run` 中注册了 `ConsoleExporter`，所以一个打日志的 guest 会向 stderr 输出一行：
   ```
   [Info] [cordis.guest] [fiber=<id>] status: ready
   ```
   这是最直接的 "surface"——你写一行，它就出现。guest 调用 `host::log(context, "info", &message)`（level 大小写不敏感；未知的 level 会映射到 `Info`）。

2. **一个 host 来询问的服务。** 插件可以 `provide` 一个服务，其 `call_service` 按需返回最新观察到的值。host 侧的组件——或另一个 guest——调用它来读取状态。这是"可读端点"意义上的 "view"：由外部的东西来拉取。

3. **一个 `EventReply`。** 当 listener 作出反应时，它返回 `ContinueValue` 或 `BreakValue`。emitter（分发该事件的提供者）收到这个 reply，并能据此行动——所以这里的 "surface" 就是 `dispatch_event` 的返回值。插件正是用这种方式，把决定或数据交回给触发事件的那一方。

4. **`cordis run` 的人类可读快照。** 这不是插件专属的东西，但 CLI 的 `inspect` 输出（`state=Active`、`dependencies=N`）是对 fiber 树的一个实时、文本形式的视图。它缓慢而静态，但确实是操作者"看见"应用的地方。

所以，"在 view 里显示"具体转化为：**声明或复用一个事件、注册一个 listener、在 `handle_event` 里作出反应，并把结果写到上面某个 surface 上。** 最有用也最简单的是日志；status/telemetry surface 则是一个持有最新值、让调用者能读取它的服务。

本章构建一个 status/telemetry 插件：一个提供者发出带消息的 `status` 事件，一个 consumer 监听并记下它。我们用服务来持有最新值，这样它也能被拉取。

## 声明（或复用）一个事件

guest SDK 中没有声明事件的宏，所以事件是一个你手工构造的 `EventId`，和 service 完全一样：

```rust
const STATUS_ABI: [u8; 32] = [0x51; 32];   // convention: agree across emitter and listener

fn status_event() -> EventId {
    EventId {
        name: "example.status".into(),
        abi_hash: STATUS_ABI.to_vec(),
    }
}
```

emitter 与 listener 必须同时就 name *和* hash 达成一致。hash 固定了 payload 契约——这里 payload 是一个 `String`（那条消息）。如果哪天你改了 payload 的类型，记得 hash 也要一起改；这正是 ABI 哈希存在的全部意义。

事件从哪来？两个选项：

- **复用现成的**——counter provider 已经有一个回显 `ContinueValue(payload)` 的 `handle_event`。你可以针对你和 emitter 商定的任意 `EventId` 注册 listener。但要注意：一个事件只有在有东西*分发*它时才有意义。provider 把机制给你；*发出（emission）*这件事要由你来写。
- **声明自己的**——像上面那样定义 `status_event()`，并从一个 provider 发出它。本章走的就是这条路：emitter 就是你的插件，所以两侧都由你掌控。

## 注册一个 listener

listener 在 `activate` 中经由 host import 注册：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(
        context,
        &status_event(),
        STATUS_LISTENER_ID,
        EventMode::Serial,
    )?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

`STATUS_LISTENER_ID` 是一个由你选的 `u64`——它是该 listener 在 host 映射里的稳定 key。`EventMode::Serial` 表示该 listener 属于一次串行（有序、遇 Break 即停）的分发。正如第 4 章所说，host 现在一次分发只路由单个 listener，所以 mode 只是传过去供 listener 自己解读，而不是让 host 去编排一次 fan-out。

有一条规则很重要：`(event, listener_id)` 组合在每个 fiber 内必须唯一。要响应两个不同的事件，就注册两个不同的 listener id。

## 从 provider 发出

provider 通过调用 `dispatch_event` 来发出：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let payload = cordis_guest::encode(&"status: ready".to_string())?;
    let reply = host::dispatch_event(
        context,
        &status_event(),
        STATUS_LISTENER_ID,
        EventMode::Serial,
        &payload,
        None,          // next_token — see below
    )?;
    // `reply` is the listener's EventReply; you could log it or act on it.
    Ok(())
}
```

emitter 指明它正在寻址的 `listener_id`。这就是第 4 章中"向特定 listener 发出"的模型——单 listener 分发边界的一个后果，你在这里会切身体会到。如果你注册了多个 listener，就按各自的 id 分别发出，或者扩展 host 的 `dispatch_event`，让它去做 fan-out。

`next_token` 是 waterfall（瀑布式）的一次性 token。`crates/cordis-core/src/event.rs` 里的原生 `WaterfallEvent` 会传递一个 `Next` continuation，让 listener 能包裹下游的结果。在 guest 路径中，`next_token` 会被一路带进 `handle_event`，但 host kernel 并不管理 token 链——解读它是 guest 自己的责任。如果你不是在构建 waterfall，就传 `None`。

## 作出反应并呈现结果

listener 的 `handle_event` 把事件变成一个可观察的结果。打日志的版本：

```rust
fn handle_event(
    context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

`host::log` 就是这个 surface。在 `cordis run` 里，它经由 `ConsoleExporter` 到达 stderr。reply 是 `ContinueValue`，因为记录日志者没有什么是决定性的要说——它观察完就继续走了。如果反过来，这个 listener 是一个可以否决或作答的*策略*，它就会返回 `BreakValue(encoded)`，而 emitter 会把这个当作结果。

### status/telemetry 服务 surface

日志是"推"：事件触发时值就出去了。*拉取式*的 view，则是插件提供的一个服务，其 `call_service` 读取最新值。把两者结合起来——listener 既打日志又更新 thread-local，而服务让调用者读取它。

```rust
thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static LATEST_STATUS: RefCell<String> = const { RefCell::new(String::new()) };
    static STATUS_SERVICE_REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
}

fn status_service() -> ServiceId {
    ServiceId { name: "example.status".into(), abi_hash: STATUS_ABI.to_vec() }
}

// In activate, also provide the service:
//   let reg = host::provide_service(context, &status_service())?;
//   STATUS_SERVICE_REGISTRATION.with(|slot| *slot.borrow_mut() = Some(reg));

fn call_service(
    _context: CallContext,
    service: ServiceId,
    method: u32,
    _payload: Vec<u8>,
) -> Result<Vec<u8>, KernelError> {
    if service.name != status_service().name || method != GET_METHOD {
        return Err(KernelError::InvalidArgument("unknown status method".into()));
    }
    let latest = LATEST_STATUS.with(|slot| slot.borrow().clone());
    cordis_guest::encode(&latest)
}

fn handle_event(
    context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    LATEST_STATUS.with(|slot| *slot.borrow_mut() = message.clone());
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

现在这个 surface 有了两个头：事件把一行**推**进日志，服务则**拉**取最新值。host 侧组件，或另一个带 `inject: [status_service()]` 的 guest，都能调用 `example.status` 按需读取最近的状态。这就是本章承诺的那个具体的 "status/telemetry surface"，而且只用了真实存在的 API。

## 事件驱动 status surface 的具体方案

综合起来，方案是：

1. **挑一个事件**——`example.status`，带消息 payload 和一个固定的 ABI 哈希。
2. **Emitter**（provider）：在 `activate` 里编码一个 `String` 并 `dispatch_event` 到该 listener id。要让它持续发出而不是一次性，就从后台循环里做——guest SDK 信任 host 去负责清理，所以一旦你有办法运行它，一个周期性发出事件的任务就是合法的模式。（host 上的 `GuestTaskGroup` 会在 teardown 时 abort 并 join host tasks。）
3. **Listener**（consumer 或同一个插件）：在 `activate` 里 `register_listener`，并在 `handle_event` 中解码 payload、更新 thread-local、用 `host::log` 记下它。
4. **拉取式 surface**：`provide` 一个 `status` 服务，其 `call_service` 返回那个 thread-local。
5. **组合**两者到 `cordis.json`，用 `isolate` 绑定让它们落在同一个 realm。

完全真实的部分：event id、`register_listener`、`dispatch_event`、`handle_event`、`host::log`、`provide_service`/`call_service`，以及 encode/decode 边界。由你提供的部分：确切的 payload 类型与哈希、循环节奏，以及日志级别。

## UI 边界止步于哪里

如果 "view" 指的是活生生的可视画面，那么诚实的界线就在这里。这个仓库**不**渲染。没有浏览器、没有 canvas、没有通向前端的 IPC。你最接近"实时视图"的是：

- `cordis run` 期间的 stderr 行（经 `host::log` → `ConsoleExporter`），
- 一份 `cordis inspect` 快照（一棵静态的、已安定的 fiber 树），
- 一个其他组件去拉取的服务端点。

构建一个真正的 HTML/UI 视图，需要这个运行时之外的东西——一个把 `host::log` 或 `status` 服务变成可渲染 surface 的 embedding host，或者一个经由其他通道调用该服务的前端。这是一道刻意的边界，不是疏漏：这个运行时是插件宿主，不是 UI 工具包。如果你想要真正的视图，插件应当*暴露数据*（以服务或事件的形式），让 embedding 层去呈现它。

这同样是正确的关注点分离。一个负责"当前状态在这里"的插件，与一个负责"把它画出来"的插件，是两个不同的关注点；运行时给你第一个，第二个是 embedding 层的职责。`host::log` / `status` 服务的做法，正是为第一个准备的、符合预期的机制。

## 试着把它弄坏

- 用一个 provider 之后从不会发出的 `listener_id` 来注册 listener。什么可见的事情都不会发生——事件永远不会在那个 id 上触发。这是缺失 provider 在事件领域的对应物：listener 已就位，却永远不会被触发，唯一的线索就是它的缺席。
- 向一个没人注册的 `listener_id` 发出。host 返回 `listener {id} is not registered`。检查两侧的 id 常量——它们必须一致，就像 service 的 hash 一样。
- 用错误的类型来解码 payload（比如用 `u64` 而不是 `String`）。`handle_event` 会从 `decode` 拿到 `InvalidArgument`，listener 失败。ABI 哈希正是为了不让这种不匹配变成一场无声的意外。

下一章：[Troubleshooting](08-troubleshooting.zh.md)——常见的失败模式以及如何读懂它们。
