# Effects

effect 子系统是 Cordis 可逆 effect 模型的中心。论文（§3.1）把一个 effect 写作
`𝔈Γ ≔ Γ → Γ × (Γ → Γ)`：运行一个 effect 会产出一个新状态，**以及**一个把状态还原回旧状态的逆。
在 Rust 中，这个逆是一个 [`Disposer`](#disposer)，而 [`EffectScope`](#effectscope) 累积逆，使 fiber
能在卸载时 `recoverΓ` —— 按 LIFO 应用它们。

整个模型就是“可逆 effect，disposer 作为逆”：组件通过返回 disposer 来执行 effects；运行时持有这些
disposer，并在拥有它们的 fiber 卸载时负责按注册顺序的逆序运行它们。disposer *并不*被校验是否真的
还原了 effect —— 论文明确说明（§5.1.1），“伴随的逆是否还原了 effect，是组件作者的责任，而非运行时
验证的性质”，本实现从设计上与此一致。

## `Disposer`

```rust
pub struct Disposer { /* opaque */ }
```

一个由 effect 捕获的异步逆操作。由一个返回 future 的闭包构造。

```rust
impl Disposer {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CordisError>> + Send + 'static;

    pub fn infallible<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static;
}
```

- `new` 包装一个可失败的逆；它的 future 返回 `Result<(), CordisError>`。
- `infallible` 包装一个 future 返回 `()` 的逆，并把任何错误都当作 `Ok`。当逆不可能失败、或者你
  没有任何有意义的错误需要上报时，使用它。

一个 `Disposer` 恰好运行一次 —— 它被移入运行 disposer 的循环并被消费掉。

## `EffectGuard`

```rust
#[derive(Clone)]
pub struct EffectGuard { /* opaque */ }
```

单个 effect 的*句柄*：你可以克隆它、检查它的状态，并最终 dispose 它。它与一个 `EffectScope` 成对
创建：

```rust
impl EffectGuard {
    pub fn new(label: impl Into<Arc<str>>) -> (Self, EffectScope);

    pub fn spawn_stream<S>(label: impl Into<Arc<str>>, stream: S) -> (Self, EffectScope)
    where
        S: Stream<Item = Result<Disposer, CordisError>> + Send + Unpin + 'static;

    pub fn id(&self) -> EffectId;
    pub fn is_armed(&self) -> bool;
    pub fn is_disposed(&self) -> bool;
    pub fn metadata(&self) -> EffectMeta;

    pub async fn dispose(&self) -> DisposeReport;
}
```

### `new`

创建一个 armed effect（状态为 `Armed`，没有 disposer）以及它的 `EffectScope`。

### `spawn_stream`

立即启动一个异步的**effect 迭代器（iterator）**。每个成功的 stream item 都是一个 `Disposer` ——
即刚完成的那一步的逆 —— 并在产生时被注册。Dispose 只在 item 边界处停止迭代器：先等待在途
（in-flight）的 item，然后运行每一个收集到的逆。stream 出错，或在 stream 中 panic，都会触发
dispose。

**Panics** —— 在 Tokio runtime 之外调用时会 panic。

### `dispose`

按**注册顺序的逆序**（LIFO）运行每个逆，然后把 effect 标记为 `Disposed`。并发调用与之后的调用会
等待，然后返回*同一个*报告 —— dispose 是幂等且恰好一次的。

**Errors** —— 在尝试过每一个 disposer 之后，返回携带全部 disposer 失败与 panic 的
`Err(DisposeErrors)`。一个失败的 disposer 绝不会跳过其余的那些。

### 恰好一次的状态机

每个 `EffectInner` 携带一个 `Mutex<EffectState>`：

```rust
enum EffectStatus {
    Armed,        // accepting disposers
    Draining,     // a stream runner is in flight; stop accepting, wait for it
    Disposing,    // running disposers now
    Disposed(DisposeReport),
}
```

- 注册一个 disposer（经由 `EffectScope::defer`）要求状态为 `Armed`；否则它返回
  `CordisError::InactiveEffect`。
- 对没有在途 stream runner 的 `Armed` effect 调用 `dispose()` 会直接转换到 `Disposing` 并运行
  disposers。当有 stream runner 在运行时会先排空（drain）该 runner。
- 在 mutex 保护下该转换是原子的，因此一个快的 disposer 不会与等待者的注册竞争（race）。

这就是论文的保证（§5.1.1）：“触发两次会在没有任何应用产生过的状态上应用一个逆”。disposer 集合
只被消费一次，第二次 `dispose()` 返回已存储的报告，而不重新运行任何东西（测试：
`dispose_is_idempotent`）。

## `EffectScope`

```rust
#[derive(Clone, Debug)]
pub struct EffectScope { /* opaque */ }
```

它常常被丢弃、改用克隆；它是 effect 内部的*注册端点*。你从 `EffectGuard::new` / `spawn_stream`
得到一个，然后把它交给 effect 主体，使它可以注册逆。

```rust
impl EffectScope {
    pub fn id(&self) -> EffectId;

    pub fn defer(&self, disposer: Disposer) -> Result<(), CordisError>;

    pub fn child(&self, label: impl Into<Arc<str>>) -> Result<(EffectGuard, EffectScope), CordisError>;
}
```

### `defer`

向该 effect 添加一个逆。

**Errors** —— 在 dispose 开始之后返回 `CordisError::InactiveEffect { effect }`。

### `child`

创建一个嵌套 effect，它是该 effect 的逆**与** metadata 树的一部分。child 被注册为 parent 的
一个 disposer，因此 dispose parent 会同时 dispose child；子 effect 的失败会以
`CordisError::ChildEffectFailed { effect, errors }` 上报。

**Errors** —— 在 dispose 开始之后返回 `CordisError::InactiveEffect { effect }`。

## `EffectSet`

```rust
#[derive(Clone, Debug)]
pub struct EffectSet { /* opaque */ }
```

拥有单个 **fiber** 的顶层 effects。一个 fiber 创建一个 `EffectSet`，并把它的全部组件/成员 effects
都注册在它下面。

```rust
impl EffectSet {
    pub fn new(label: impl Into<Arc<str>>) -> Self;
    pub fn id(&self) -> EffectId;
    pub fn effect(&self, label: impl Into<Arc<str>>) -> Result<(EffectGuard, EffectScope), CordisError>;
    pub fn metadata(&self) -> Vec<EffectMeta>;
    pub async fn dispose(&self) -> DisposeReport;
}
```

- `effect` 创建由本 set 拥有的一个顶层 effect（委托给 `scope.child`）。
- `metadata` 为每个存活的 effect 返回一个 `EffectMeta`。
- `dispose` 按创建顺序的逆序 dispose 每一个所拥有的 effect；并发调用与之后的调用返回同一个报告。

**Errors** —— 在 set 的 dispose 开始之后，`effect` 返回 `InactiveEffect`；在所有 effect 都被
尝试之后，`dispose` 返回聚合的子 effect 失败（以 `DisposeErrors` 形式）。

## `DisposeReport` 与 `DisposeErrors`

```rust
pub type DisposeReport = Result<(), DisposeErrors>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposeErrors { /* opaque */ }
impl DisposeErrors {
    pub fn errors(&self) -> &[CordisError];
}
```

一次 dispose 运行的报告。`Ok(())` 表示每个逆都无错误地运行了。`Err(DisposeErrors)` 收集一切出错
之处 —— 包括被转换为 `CordisError::DisposerPanic` 的 panic —— 但仅当**每一个** disposer 都有机会
运行之后才这样做。`DisposeErrors` 实现了 `Display`（“N disposer(s) failed”）和
`std::error::Error`。

## `EffectMeta`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectMeta {
    pub id: EffectId,
    pub label: Arc<str>,
    pub children: Vec<EffectMeta>,
}
```

一个用于暴露嵌套 effect 标签以便诊断的树节点，例如 fiber 的 `EffectSet` 上的 `metadata()`。每个
节点包含其 effect id、它的 label，以及它（仍然存活）的 children。label 是人类可读的，例如
`"method:start"`、`"dynamic:example.counter"` 或 `"timer:interval"`。

## LIFO 恢复与论文对应

- `run_disposers` 以 `disposers.into_iter().rev()` 迭代 —— 逆按注册顺序的逆序应用（论文定理 16；
  测试 `disposers_run_in_lifo_order`）。
- 恢复**越过失败的逆继续** —— 返回 `Err` 或 panic 的 disposer 会被记录，循环继续执行（测试
  `failures_do_not_skip_remaining_disposers`）。
- `EffectScope::defer` 把内容 push 进一个 `Vec` 就是 `trackΓ`；`run_disposers` 就是 `recoverΓ`。
- `spawn_stream` 就是论文的 effect 迭代器 `ℑΓ`（定义 17/18）：每个 stream item 都是一个逆。

## 错误

- `InactiveEffect { effect }` —— dispose 开始之后的 `defer`/`child`/`effect`。
- `DisposeErrors` —— 在 `dispose` 期间有一个或多个逆失败或 panic。
- `ChildEffectFailed { effect, errors }` —— 某个子 effect 在它的 parent 被 dispose 期间失败。
- `DisposerPanic { message }` / `DisposerFailed { message }` —— 记录在 `DisposeErrors` 内部。
