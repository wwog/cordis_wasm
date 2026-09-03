# Timers (`cordis-timer`)

`cordis-timer` provides effect-owned timers: one-shot delays, intervals, debounced/throttled
schedulers, and a cancellation-aware sleep, all bound to an `EffectScope`. A timer's lifetime is the
effect scope it was created from — when the owning fiber unloads and the scope disposes, the timer is
cancelled and its task joined, so no timer leaks past its component. This is the paper's revertible
effect applied to time: creating a timer is an acquisition, and its inverse (abort + join the task) is
the disposer the scope runs on cleanup.

## `TimerError`

```rust
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimerError {
    #[error("timer context was disposed")] ContextDisposed,
    #[error("timer cleanup failed: {0}")] Cleanup(String),
}
```

`ContextDisposed` is returned when you try to use a timer after its scope has begun disposal;
`Cleanup` wraps a failed effect disposal during explicit `cancel`/`close`.

## `timeout`

```rust
pub fn timeout<F, Fut>(scope: &EffectScope, delay: Duration, callback: F) -> Result<EffectGuard, CordisError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

Starts a one-shot callback owned by `scope`. After `delay` elapses, `callback()` is awaited once. The
returned `EffectGuard` lets you cancel it by disposing the timer's effect.

**Errors** — `CordisError::InactiveEffect` when the scope is disposing.

## `interval`

```rust
pub fn interval<F, Fut>(scope: &EffectScope, period: Duration, mut callback: F) -> Result<EffectGuard, CordisError>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static;
```

Starts a repeated callback owned by `scope`. `callback` runs on every period after the first full
period (`ticker.tick()` is called once to advance past the immediate first tick). The loop never
returns on its own; disposing the returned `EffectGuard` (or the parent effect) aborts the task.

**Errors** — `CordisError::InactiveEffect` when the scope is disposing.

## `IntervalStream`

```rust
pub struct IntervalStream { /* receiver: mpsc::Receiver<Instant>, effect: Option<EffectGuard> */ }
impl IntervalStream {
    pub async fn close(mut self) -> Result<(), TimerError>;
}
impl Stream for IntervalStream { type Item = Instant; ... }
impl Drop for IntervalStream { /* dispose in background */ }
```

A bounded (capacity-1) stream of interval ticks owned by `scope`. The first item arrives after one
full period. Slow consumers apply **backpressure** (the mpsc channel is size 1); missed Tokio ticks
are **skipped** (`MissedTickBehavior::Skip`), never replayed in a burst.

- `close` stops the interval and waits for its task to finish. **Errors** —
  `TimerError::Cleanup` if effect cleanup fails.
- `Drop` disposes the effect in the background (if a Tokio runtime is available), so the stream ends
  when the parent scope disposes. Test: `interval_stream_ends_when_its_parent_is_disposed`.

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

Creates a **trailing-edge** debouncer. Each `call(value)` replaces the pending value and restarts the
quiet period; after no calls arrive for `delay`, the callback runs once with the **latest** value.

Returns a `Debouncer<T>`:

```rust
pub struct Debouncer<T> { /* pending, changed, effect */ }
impl<T> Debouncer<T> {
    pub fn call(&self, value: T) -> Result<(), TimerError>;
    pub async fn cancel(mut self) -> Result<(), TimerError>;
}
impl<T> Drop for Debouncer<T> { /* dispose in background */ }
```

**Errors** — `call`: `TimerError::ContextDisposed` after cancellation begins. `cancel`:
`TimerError::Cleanup` if effect cleanup fails.

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

Creates a **leading-edge** throttle with one latest trailing value per period. The first `call` fires
immediately; subsequent calls during the period replace the pending value; when the period elapses, the
latest value fires, then the throttle goes quiet until the next `call`.

Returns a `Throttler<T>` with the same `call`/`cancel` surface as `Debouncer`.

**Errors** — `call`: `TimerError::ContextDisposed` after cancellation begins. `cancel`:
`TimerError::Cleanup` if effect cleanup fails.

## `sleep`

```rust
pub async fn sleep(scope: &EffectScope, delay: Duration) -> Result<(), TimerError>;
```

Sleeps until the deadline or parent scope disposal, whichever comes first. It creates a child effect
on `scope`, defers a disposer that sends a cancel signal, and `select!`s between `sleep(delay)` and the
cancel signal. If the parent disposes first, `sleep` returns `TimerError::ContextDisposed`.

**Errors** — `TimerError::ContextDisposed` when cleanup wins the race, or a cleanup error when the
child effect cannot settle.

## Effect-owned cancellation

Every timer registers a `Disposer` into the given `EffectScope` (via `spawn_owned`, which uses
`scope.child` then defers a disposer that aborts and awaits the task). So whether the component
explicitly disposes the returned `EffectGuard`, or the whole fiber's `EffectSet` disposes on unload,
the timer's task is aborted and joined. The tests `timeout_and_interval_are_cancelled_by_effect_disposal`
and `sleep_reports_parent_disposal` confirm: after the owner disposes, further ticks/calls do nothing
(or report `ContextDisposed`), and a pending sleep returns `ContextDisposed`.

## Example

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

## Errors

- `CordisError::InactiveEffect` — creating a timer after the scope began disposing.
- `TimerError::ContextDisposed` — using a timer after it began cancelling.
- `TimerError::Cleanup(String)` — explicit `cancel`/`close` failed to dispose its effect.
