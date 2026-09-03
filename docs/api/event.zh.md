# 事件

事件子系统是 Cordis 的 dispatch 表面。**事件（event）**是带固定 **dispatch 模式**的具名、类型化
声明（trait）。监听器针对 `EffectScope` 注册——因此事件监听器总是 effect 所有，并在所属 fiber
卸载时被拆除。Native 与 WebAssembly 组件共享同一个 `EventId`、同一个 `EventMode`、以及同一个
MessagePack payload codec。

共有五种 dispatch 模式，每种都是语义各异的独立 runtime 类型。宏 `#[cordis::event]` 从模式中挑选
runtime 类型并生成一个 `dispatch` 函数（见 [macros](macros.zh.md)）。

## `EventId`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EventId { /* name: Arc<str>, abi_hash: [u8; 32] */ }

impl EventId {
    pub fn new(name: impl Into<Arc<str>>, abi_hash: [u8; 32]) -> Self;
    pub fn name(&self) -> &str;
    pub const fn abi_hash(&self) -> &[u8; 32];
}
```

native 与 WebAssembly 组件共享的稳定事件标识。`name` + 32 字节 ABI hash，与 `ServiceId` 完全
一致。`Display` 的格式是 `{name}@{first-4-bytes}`。

## `EventSpec`

```rust
pub trait EventSpec: Send + Sync + 'static {
    type Input: Clone + Serialize + DeserializeOwned + Send + 'static;
    type Output: Serialize + DeserializeOwned + Send + 'static;

    const NAME: &'static str;
    const ABI_HASH: [u8; 32];
    const MODE: EventMode;

    fn event_id() -> EventId { EventId::new(Self::NAME, Self::ABI_HASH) }
}
```

为事件声明生成的静态标识与 payload 类型。宏的标记类型实现它。

## `EventMode`

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventMode {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
}
```

由事件声明选定的 dispatch 语义。五个变体映射到五个 runtime 类型，以及 TS `DispatchMode`
（'emit' | 'parallel' | 'serial' | 'bail' | 'waterfall'）。

## `EventTarget`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventTarget {
    Global,
    Realm(RealmId),
}
```

用于为一次 dispatch 选定监听器的 realm。global 监听器匹配任意 dispatch target；realm 监听器只
匹配指向相同 `RealmId` 的 dispatch。

## `ListenerOptions`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerOptions {
    pub target: EventTarget,
    pub prepend: bool,
}
impl ListenerOptions {
    pub const fn global() -> Self;
    pub const fn realm(realm: RealmId) -> Self;
    #[must_use] pub const fn prepend(mut self) -> Self;
}
impl Default for ListenerOptions { /* global, no prepend */ }
```

注册期的监听器选择与排序。`prepend` 把监听器放到已为同一 target 注册的监听器之前。

## 五种 dispatch 模式

| 模式 | Runtime | 语义 |
|---|---|---|
| `Emit` | `AsyncEvent` | Fire-and-forget：按顺序启动匹配的监听器，不等待。错误/panic 进入 error sink。 |
| `Parallel` | `AsyncEvent` | 并发运行所有匹配的监听器；全部等待；结果中保留注册顺序。聚合错误。 |
| `Serial` | `AsyncEvent` | 按顺序运行监听器，在第一个 `Break` 处停止；返回该值。返回第一个错误。 |
| `Bail` | `BailEvent` | serial 的同步版本：按顺序调用监听器，在第一个 `Break` 处停止。 |
| `Waterfall` | `WaterfallEvent` | 洋葱中间件：每个监听器把链的其余部分包裹在最终的 `next` 周围。 |

## `AsyncEvent<P, B>`

```rust
#[derive(Clone)]
pub struct AsyncEvent<P: 'static, B: 'static> { /* opaque */ }
impl<P, B> AsyncEvent<P, B> {
    pub fn new() -> Self;

    pub fn listen<F, Fut>(
        &self, effect: &EffectScope, options: ListenerOptions, callback: F,
    ) -> Result<ListenerId, CordisError>
    where
        F: Fn(P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ControlFlow<B>, CordisError>> + Send + 'static;
}
impl<P, B> AsyncEvent<P, B> where P: Clone + Send + 'static, B: Send + 'static {
    pub async fn parallel(&self, target: EventTarget, payload: &P) -> Result<Vec<ControlFlow<B>>, CordisError>;
    pub async fn serial(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError>;
    pub fn emit_nowait<S>(&self, target: EventTarget, payload: &P, error_sink: S) -> Result<(), CordisError>
    where S: Fn(CordisError) + Send + Sync + 'static;
}
```

- `listen` 注册一个**由 `effect` 所有**的监听器——handler 的逆被 defer 进该 scope，因此 effect
  所有的监听器会在 fiber 卸载时被移除。返回一个 `ListenerId`。
- `parallel` 通过 `join_all` 并发运行所有匹配的监听器；输出中保留注册顺序。任何错误/panic 都会被
  收集；如果有，则返回 `CordisError::EventListenersFailed { errors }`。
- `serial` 按顺序运行，在第一个 `ControlFlow::Break` 处停止并返回其值。
- `emit_nowait` 不等待、按顺序启动匹配的监听器；异步失败与 panic 被投递给 `error_sink`。如果调用
  监听器在其产生 future 之前就 panic，则立即返回——已启动的监听器会继续运行。

**错误** —— `listen`：若 dispose 已开始，返回 `InactiveEffect`。`parallel`：
`EventListenersFailed`。`serial` 与 `emit_nowait`：返回第一个监听器错误或 panic（对 `emit_nowait`
而言只有同步的那些；异步的进入 sink）。

**Panics** —— 在 Tokio runtime 之外调用 `emit_nowait` 会 panic。

## `BailEvent<P, B>`

```rust
#[derive(Clone)]
pub struct BailEvent<P: 'static, B: 'static> { /* opaque */ }
impl<P, B> BailEvent<P, B> {
    pub fn new() -> Self;
    pub fn listen<F>(&self, effect: &EffectScope, options: ListenerOptions, callback: F)
        -> Result<ListenerId, CordisError>
    where F: Fn(P) -> Result<ControlFlow<B>, CordisError> + Send + Sync + 'static;
}
impl<P, B: 'static> BailEvent<P, B> where P: Clone + 'static {
    pub fn bail(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError>;
}
```

用于确定性 bail dispatch 的 effect 所有同步事件。`bail` 按顺序运行监听器并在第一个 `Break` 处
停止；其监听器回调是**同步**的（返回 `Result<
ControlFlow<B>, CordisError>`，而非 future）。这正是 `bail` 之所以确定且非 async 的原因。

**错误** —— `listen`：`InactiveEffect`。`bail`：返回第一个监听器错误或 panic
（`CordisError::EventListenerPanicked`）。

## `WaterfallEvent<T>`

```rust
#[derive(Clone)]
pub struct WaterfallEvent<T: Send + 'static> { /* opaque */ }
impl<T: Send + 'static> WaterfallEvent<T> {
    pub fn new() -> Self;
    pub fn listen<F, Fut>(&self, effect: &EffectScope, options: ListenerOptions, callback: F)
        -> Result<ListenerId, CordisError>
    where
        F: Fn(T, Next<T>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, CordisError>> + Send + 'static;
    pub async fn run(&self, target: EventTarget, value: T) -> Result<T, CordisError>;
}
```

effect 所有的洋葱中间件。每个监听器接收该值和一个 `Next` 延续；调用 `next.call(value)` 会调用链
的其余部分，监听器可以在之前与之后转换该值。`run` 把匹配的监听器折叠成一条链并运行它。

**错误** —— `listen`：`InactiveEffect`。`run`：传播监听器错误，并把 panic 转成
`CordisError::EventListenerPanicked`。

## `Next<T>`

```rust
pub struct Next<T: Send + 'static> { /* step: Option<WaterfallStep<T>> */ }
impl<T: Send + 'static> Next<T> {
    pub async fn call(&mut self, value: T) -> Result<T, CordisError>;
}
```

传给 waterfall 监听器的 one-shot 延续。`call` **恰好一次**调用 waterfall 的其余部分：对同一个
`Next` 的第二次调用返回 `CordisError::NextAlreadyUsed`，因为把链绕回自身会导致递归。

**错误** —— 第二次调用返回 `CordisError::NextAlreadyUsed`，或返回下游监听器的错误。

## `ListenerId`

```rust
pub struct ListenerId(u64);   // from cordis_core::id
```

一次注册的稳定标识。公开出来让调用方能引用（并移除）一个监听器。

## `ControlFlow` 的语义

每个非 waterfall 的监听器都返回（或包装一个返回该值的 future）`std::ops::ControlFlow<B>`：

- `ControlFlow::Continue(())` —— 监听器已运行，dispatch 应继续。
- `ControlFlow::Break(value)` —— 监听器在 bail；dispatch 停止并返回 `Some(value)`
  （serial/bail）——`parallel` 保留所有 flow 并以 `Vec` 返回它们。

`B` 是事件的 `Output`。`ControlFlow` 让监听器要么正常参与、要么带值短路，而不会把"运行良好"与
"运行了但想停下"混为一谈。

## Event payload codec（载荷编解码）

```rust
pub fn encode_event_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, CordisError>;
pub fn decode_event_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CordisError>;
```

用规范 `MessagePack` codec 编码/解码一个 event payload。

**错误** —— `encode`：`CordisError::EventEncodeFailed { message }`；`decode`：
`CordisError::EventDecodeFailed { message }`。

## 示例

```rust
let event = AsyncEvent::<u64, ()>::new();
let (owner, scope) = EffectGuard::new("listener");
event.listen(&scope, ListenerOptions::realm(realm), move |value| async move {
    println!("got {value}");
    Ok(ControlFlow::Continue(()))
})?;

// serial: stop at the first Break
let result = event.serial(EventTarget::Realm(realm), &42).await?;
```

来自源码测试的 waterfall 示例：

```rust
let event = WaterfallEvent::<i32>::new();
event.listen(&scope, ListenerOptions::global(), move |value, mut next| async move {
    let value = next.call(value + 1).await?;
    Ok(value + 1)
})?;
assert_eq!(event.run(EventTarget::Global, 0).await.unwrap(), 22);
```

## 错误

- `InactiveEffect` —— `listen` 在 dispose 开始之后被调用。
- `EventListenersFailed { errors }` —— `parallel` 遇到任意监听器错误/panic。
- `EventListenerPanicked { message }` —— 有监听器 panic。
- `NextAlreadyUsed` —— 一个 waterfall `next` 被调用了两次。
