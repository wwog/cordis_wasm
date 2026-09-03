# 定时器（`cordis-timer`）

`cordis-timer` 提供随 effect（effect-owned）的定时器：一次性延迟、间隔（interval）、防抖/节流的调度器，以及感知取消的 sleep，全部绑定到一个 `EffectScope`。定时器的生命周期就是创建它的 effect scope——当拥有它的 fiber 卸载、scope 被 dispose 时，定时器会被取消并 join 其任务，因此不会有定时器泄漏到它的组件之外。这是论文的可逆（revertible）effect 应用于时间：创建定时器是一次获取（acquisition），而其逆（abort 并 join 该任务）就是 scope 在清理时运行的 disposer。

## `TimerError`

```rust
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimerError {
    #[error("timer context was disposed")] ContextDisposed,
    #[error("timer cleanup failed: {0}")] Cleanup(String),
}
```

当你在其 scope 已开始 disposal 之后尝试使用某个定时器时，会返回 `ContextDisposed`；`Cleanup` 包装显式 `cancel`/`close` 期间一次失败的 effect disposal。

## `timeout`

```rust
pub fn timeout<F, Fut>(scope: &EffectScope, delay: Duration, callback: F) -> Result<EffectGuard, CordisError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

启动一个由 `scope` 拥有的一次性 callback。`delay` 过后，`callback()` 被 await 一次。返回的 `EffectGuard` 让你可以通过 dispose 该定时器的 effect 来取消它。

**错误** —— scope 正在 disposing 时返回 `CordisError::InactiveEffect`。

## `interval`

```rust
pub fn interval<F, Fut>(scope: &EffectScope, period: Duration, mut callback: F) -> Result<EffectGuard, CordisError>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

启动一个由 `scope` 拥有的重复 callback。在第一个完整周期之后，`callback` 每个周期运行一次（`ticker.tick()` 被调用一次，以越过紧邻的第一个 tick）。该循环不会自行返回；dispose 返回的 `EffectGuard`（或父 effect）会 abort 该任务。

**错误** —— scope 正在 disposing 时返回 `CordisError::InactiveEffect`。

## `IntervalStream`

```rust
pub struct IntervalStream { /* receiver: mpsc::Receiver<Instant>, effect: Option<EffectGuard> */ }
impl IntervalStream {
    pub async fn close(mut self) -> Result<(), TimerError>;
}
impl Stream for IntervalStream { type Item = Instant; ... }
impl Drop for IntervalStream { /* dispose in background */ }
```

`scope` 拥有的一段有界（容量 1）的 interval tick 流。第一个 item 在一个完整周期后到达。消费较慢的消费者会施加**背压（backpressure）**（mpsc channel 大小为 1）；错过的 Tokio tick 会被**跳过**（`MissedTickBehavior::Skip`），绝不会 burst 重放。

- `close` 停止 interval 并等待其任务结束。**错误** —— effect 清理失败时为 `TimerError::Cleanup`。
- `Drop` 在后台 dispose 该 effect（若可用 Tokio runtime），因此当父 scope 被 dispose 时流即结束。测试：`interval_stream_ends_when_its_parent_is_disposed`。

## `debounce`

```rust
pub fn debounce<T, F, Fut>(
    scope: &EffectScope, delay: Duration, mut callback: F,
) -> Result<Debouncer<T>, CordisError>
where
    T: Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

创建一个**尾沿（trailing-edge）**防抖器。每次 `call(value)` 都会替换挂起的值并重新开始静默周期；当 `delay` 内不再有调用到达时，callback 以**最新**的值运行一次。

返回一个 `Debouncer<T>`：

```rust
pub struct Debouncer<T> { /* pending, changed, effect */ }
impl<T> Debouncer<T> {
    pub fn call(&self, value: T) -> Result<(), TimerError>;
    pub async fn cancel(mut self) -> Result<(), TimerError>;
}
impl<T> Drop for Debouncer<T> { /* dispose in background */ }
```

**错误** —— `call`：取消开始后为 `TimerError::ContextDisposed`。`cancel`：effect 清理失败时为 `TimerError::Cleanup`。

## `throttle`

```rust
pub fn throttle<T, F, Fut>(
    scope: &EffectScope, period: Duration, mut callback: F,
) -> Result<Throttler<T>, CordisError>
where
    T: Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

创建一个**前沿（leading-edge）**节流器，每个周期至多有一个最新的尾值。第一次 `call` 立即触发；周期内的后续调用替换挂起的值；当周期结束时，最新值触发，然后节流器安静下来，直到下一次 `call`。

返回一个 `Throttler<T>`，其 `call`/`cancel` 表面与 `Debouncer` 相同。

**错误** —— `call`：取消开始后为 `TimerError::ContextDisposed`。`cancel`：effect 清理失败时为 `TimerError::Cleanup`。

## `sleep`

```rust
pub async fn sleep(scope: &EffectScope, delay: Duration) -> Result<(), TimerError>;
```

睡眠直到 deadline 或父 scope 被 dispose，以先到者为准。它在 `scope` 上创建一个子 effect，defer 一个发送取消信号的 disposer，并在 `sleep(delay)` 与取消信号之间做 `select!`。如果父 scope 先被 dispose，`sleep` 返回 `TimerError::ContextDisposed`。

**错误** —— 清理赢得竞争时返回 `TimerError::ContextDisposed`，或当子 effect 无法 settle 时返回一次清理错误。

## effect 拥有的取消

每个定时器都会把一个 `Disposer` 注册进给定的 `EffectScope`（通过 `spawn_owned`，它先使用 `scope.child`，再 defer 一个 abort 并 await 该任务的 disposer）。因此，无论是组件显式 dispose 返回的 `EffectGuard`，还是整个 fiber 的 `EffectSet` 在卸载时 dispose，该定时器的任务都会被 abort 并 join。测试 `timeout_and_interval_are_cancelled_by_effect_disposal` 与 `sleep_reports_parent_disposal` 证实：owner dispose 之后，进一步的 ticks/calls 不做任何事（或报告 `ContextDisposed`），而一个挂起的 sleep 会返回 `ContextDisposed`。

## 示例

```rust
let (owner, scope) = EffectGuard::new("fiber");

// one-shot after 5s
timeout(&scope, Duration::from_secs(5), || async { println!("hi") })?;

// repeat every 1s
let ticks = Arc::new(AtomicUsize::new(0));
let t = ticks.clone();
interval(&scope, Duration::from_secs(1), move || { let t = t.clone();
    async move { t.fetch_add(1, Ordering::SeqCst); } })?;

// cancel a debouncer when the fiber unloads
let d = debounce(&scope, Duration::from_millis(100), |v: u64| async {
    println!("debounced {v}");
})?;

// cleanup happens automatically when `owner.dispose().await` runs
```

## 错误

- `CordisError::InactiveEffect` —— 在 scope 开始 disposing 之后创建定时器。
- `TimerError::ContextDisposed` —— 在定时器开始取消之后使用它。
- `TimerError::Cleanup(String)` —— 显式 `cancel`/`close` 未能 dispose 其 effect。
