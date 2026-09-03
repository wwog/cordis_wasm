//! Effect-owned timers that are cancelled and joined during Fiber cleanup.

use cordis_core::{CordisError, Disposer, EffectGuard, EffectScope};
use futures::Stream;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimerError {
    #[error("timer context was disposed")]
    ContextDisposed,
    #[error("timer cleanup failed: {0}")]
    Cleanup(String),
}

/// Effect-owned interval ticks. Parent disposal or [`Self::close`] ends the stream.
pub struct IntervalStream {
    receiver: tokio::sync::mpsc::Receiver<Instant>,
    effect: Option<EffectGuard>,
}

impl std::fmt::Debug for IntervalStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IntervalStream")
            .field("closed", &self.receiver.is_closed())
            .finish_non_exhaustive()
    }
}

impl IntervalStream {
    /// Stops this interval and waits for its task to finish.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::Cleanup`] if effect cleanup fails.
    pub async fn close(mut self) -> Result<(), TimerError> {
        dispose_timer(self.effect.take()).await
    }
}

impl Stream for IntervalStream {
    type Item = Instant;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().receiver.poll_recv(context)
    }
}

impl Drop for IntervalStream {
    fn drop(&mut self) {
        dispose_in_background(self.effect.take());
    }
}

/// A latest-value trailing-edge scheduler owned by an effect scope.
pub struct Debouncer<T> {
    pending: Arc<Mutex<Option<T>>>,
    changed: Arc<tokio::sync::Notify>,
    effect: Option<EffectGuard>,
}

impl<T> std::fmt::Debug for Debouncer<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Debouncer")
            .field(
                "active",
                &self.effect.as_ref().is_some_and(EffectGuard::is_armed),
            )
            .finish_non_exhaustive()
    }
}

impl<T> Debouncer<T> {
    /// Replaces the pending value and restarts the quiet period.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::ContextDisposed`] after cancellation begins.
    pub fn call(&self, value: T) -> Result<(), TimerError> {
        schedule_latest(&self.effect, &self.pending, &self.changed, value)
    }

    /// Cancels the scheduler and drops any pending value.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::Cleanup`] if effect cleanup fails.
    pub async fn cancel(mut self) -> Result<(), TimerError> {
        dispose_timer(self.effect.take()).await
    }
}

impl<T> Drop for Debouncer<T> {
    fn drop(&mut self) {
        dispose_in_background(self.effect.take());
    }
}

/// A leading-edge scheduler with one latest trailing value per period.
pub struct Throttler<T> {
    pending: Arc<Mutex<Option<T>>>,
    changed: Arc<tokio::sync::Notify>,
    effect: Option<EffectGuard>,
}

impl<T> std::fmt::Debug for Throttler<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Throttler")
            .field(
                "active",
                &self.effect.as_ref().is_some_and(EffectGuard::is_armed),
            )
            .finish_non_exhaustive()
    }
}

impl<T> Throttler<T> {
    /// Schedules a value, replacing an already pending trailing value.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::ContextDisposed`] after cancellation begins.
    pub fn call(&self, value: T) -> Result<(), TimerError> {
        schedule_latest(&self.effect, &self.pending, &self.changed, value)
    }

    /// Cancels the scheduler and drops any pending value.
    ///
    /// # Errors
    ///
    /// Returns [`TimerError::Cleanup`] if effect cleanup fails.
    pub async fn cancel(mut self) -> Result<(), TimerError> {
        dispose_timer(self.effect.take()).await
    }
}

impl<T> Drop for Throttler<T> {
    fn drop(&mut self) {
        dispose_in_background(self.effect.take());
    }
}

/// Starts a one-shot callback owned by `scope`.
///
/// # Errors
///
/// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
pub fn timeout<F, Fut>(
    scope: &EffectScope,
    delay: Duration,
    callback: F,
) -> Result<EffectGuard, CordisError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    spawn_owned(scope, "timer:timeout", async move {
        tokio::time::sleep(delay).await;
        callback().await;
    })
}

/// Starts a repeated callback owned by `scope`.
///
/// # Errors
///
/// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
pub fn interval<F, Fut>(
    scope: &EffectScope,
    period: Duration,
    mut callback: F,
) -> Result<EffectGuard, CordisError>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    spawn_owned(scope, "timer:interval", async move {
        let mut ticker = tokio::time::interval(period);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            callback().await;
        }
    })
}

/// Creates a bounded stream of interval ticks owned by `scope`.
///
/// The first item arrives after one full period. Slow consumers apply backpressure,
/// and missed Tokio ticks are skipped rather than replayed in a burst.
///
/// # Errors
///
/// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
pub fn interval_stream(
    scope: &EffectScope,
    period: Duration,
) -> Result<IntervalStream, CordisError> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let effect = spawn_owned(scope, "timer:interval-stream", async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            let tick = ticker.tick().await;
            if sender.send(tick).await.is_err() {
                break;
            }
        }
    })?;
    Ok(IntervalStream {
        receiver,
        effect: Some(effect),
    })
}

/// Creates an effect-owned trailing-edge debouncer.
///
/// Each call replaces the pending value. The callback runs after no calls have
/// arrived for `delay`.
///
/// # Errors
///
/// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
pub fn debounce<T, F, Fut>(
    scope: &EffectScope,
    delay: Duration,
    mut callback: F,
) -> Result<Debouncer<T>, CordisError>
where
    T: Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let pending = Arc::new(Mutex::new(None));
    let changed = Arc::new(tokio::sync::Notify::new());
    let worker_pending = pending.clone();
    let worker_changed = changed.clone();
    let effect = spawn_owned(scope, "timer:debounce", async move {
        loop {
            worker_changed.notified().await;
            loop {
                tokio::select! {
                    () = tokio::time::sleep(delay) => break,
                    () = worker_changed.notified() => {}
                }
            }
            let value = worker_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(value) = value {
                callback(value).await;
            }
        }
    })?;
    Ok(Debouncer {
        pending,
        changed,
        effect: Some(effect),
    })
}

/// Creates an effect-owned leading-edge throttle with latest-value trailing calls.
///
/// # Errors
///
/// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
pub fn throttle<T, F, Fut>(
    scope: &EffectScope,
    period: Duration,
    mut callback: F,
) -> Result<Throttler<T>, CordisError>
where
    T: Send + 'static,
    F: FnMut(T) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let pending = Arc::new(Mutex::new(None));
    let changed = Arc::new(tokio::sync::Notify::new());
    let worker_pending = pending.clone();
    let worker_changed = changed.clone();
    let effect = spawn_owned(scope, "timer:throttle", async move {
        loop {
            worker_changed.notified().await;
            loop {
                let value = worker_pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(value) = value {
                    callback(value).await;
                }
                tokio::time::sleep(period).await;
                if worker_pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                {
                    break;
                }
            }
        }
    })?;
    Ok(Throttler {
        pending,
        changed,
        effect: Some(effect),
    })
}

/// Sleeps until the deadline or parent scope disposal.
///
/// # Errors
///
/// Returns [`TimerError::ContextDisposed`] when cleanup wins the race, or a
/// cleanup error when the child effect cannot settle.
pub async fn sleep(scope: &EffectScope, delay: Duration) -> Result<(), TimerError> {
    let (effect, child) = scope
        .child("timer:sleep")
        .map_err(|_| TimerError::ContextDisposed)?;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let cancel = Arc::new(Mutex::new(Some(cancel)));
    child
        .defer(Disposer::infallible(move || async move {
            if let Some(cancel) = cancel
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = cancel.send(());
            }
        }))
        .map_err(|_| TimerError::ContextDisposed)?;

    tokio::select! {
        () = tokio::time::sleep(delay) => {
            effect.dispose().await.map_err(|error| TimerError::Cleanup(error.to_string()))
        }
        _ = cancelled => Err(TimerError::ContextDisposed),
    }
}

fn spawn_owned(
    scope: &EffectScope,
    label: &'static str,
    future: impl Future<Output = ()> + Send + 'static,
) -> Result<EffectGuard, CordisError> {
    let (effect, child) = scope.child(label)?;
    let task = Arc::new(Mutex::new(Some(tokio::spawn(future))));
    let cleanup_task = task.clone();
    if let Err(error) = child.defer(Disposer::infallible(move || async move {
        let task = cleanup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    })) {
        if let Some(task) = task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        return Err(error);
    }
    Ok(effect)
}

fn schedule_latest<T>(
    effect: &Option<EffectGuard>,
    pending: &Mutex<Option<T>>,
    changed: &tokio::sync::Notify,
    value: T,
) -> Result<(), TimerError> {
    if !effect.as_ref().is_some_and(EffectGuard::is_armed) {
        return Err(TimerError::ContextDisposed);
    }
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(value);
    changed.notify_one();
    Ok(())
}

async fn dispose_timer(effect: Option<EffectGuard>) -> Result<(), TimerError> {
    let Some(effect) = effect else {
        return Ok(());
    };
    effect
        .dispose()
        .await
        .map_err(|error| TimerError::Cleanup(error.to_string()))
}

fn dispose_in_background(effect: Option<EffectGuard>) {
    let Some(effect) = effect else {
        return;
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = effect.dispose().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test(start_paused = true)]
    async fn timeout_and_interval_are_cancelled_by_effect_disposal() {
        let (owner, scope) = EffectGuard::new("owner");
        let calls = Arc::new(AtomicUsize::new(0));
        let timeout_calls = calls.clone();
        timeout(&scope, Duration::from_secs(5), move || async move {
            timeout_calls.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let interval_calls = calls.clone();
        interval(&scope, Duration::from_secs(1), move || {
            let interval_calls = interval_calls.clone();
            async move {
                interval_calls.fetch_add(1, Ordering::SeqCst);
            }
        })
        .unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        let before_dispose = calls.load(Ordering::SeqCst);
        owner.dispose().await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(calls.load(Ordering::SeqCst), before_dispose);
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_reports_parent_disposal() {
        let (owner, scope) = EffectGuard::new("owner");
        let sleeper = tokio::spawn(async move { sleep(&scope, Duration::from_secs(10)).await });
        tokio::task::yield_now().await;
        owner.dispose().await.unwrap();
        assert_eq!(sleeper.await.unwrap(), Err(TimerError::ContextDisposed));
    }

    #[tokio::test(start_paused = true)]
    async fn interval_stream_ends_when_its_parent_is_disposed() {
        let (owner, scope) = EffectGuard::new("owner");
        let mut ticks = interval_stream(&scope, Duration::from_secs(1)).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(ticks.next().await.is_some());

        owner.dispose().await.unwrap();
        assert_eq!(ticks.next().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn debounce_keeps_only_the_latest_value() {
        let (owner, scope) = EffectGuard::new("owner");
        let values = Arc::new(Mutex::new(Vec::new()));
        let callback_values = values.clone();
        let debouncer = debounce(&scope, Duration::from_secs(1), move |value| {
            let callback_values = callback_values.clone();
            async move {
                callback_values.lock().unwrap().push(value);
            }
        })
        .unwrap();

        debouncer.call(1).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        debouncer.call(2).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(999)).await;
        tokio::task::yield_now().await;
        assert!(values.lock().unwrap().is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*values.lock().unwrap(), vec![2]);

        owner.dispose().await.unwrap();
        assert_eq!(debouncer.call(3), Err(TimerError::ContextDisposed));
    }

    #[tokio::test(start_paused = true)]
    async fn throttle_runs_leading_and_latest_trailing_values() {
        let (owner, scope) = EffectGuard::new("owner");
        let values = Arc::new(Mutex::new(Vec::new()));
        let callback_values = values.clone();
        let throttler = throttle(&scope, Duration::from_secs(1), move |value| {
            let callback_values = callback_values.clone();
            async move {
                callback_values.lock().unwrap().push(value);
            }
        })
        .unwrap();

        throttler.call(1).unwrap();
        tokio::task::yield_now().await;
        throttler.call(2).unwrap();
        throttler.call(3).unwrap();
        assert_eq!(*values.lock().unwrap(), vec![1]);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(*values.lock().unwrap(), vec![1, 3]);

        owner.dispose().await.unwrap();
        assert_eq!(throttler.call(4), Err(TimerError::ContextDisposed));
    }
}
