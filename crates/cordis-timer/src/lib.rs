//! Effect-owned timers that are cancelled and joined during Fiber cleanup.

use cordis_core::{CordisError, Disposer, EffectGuard, EffectScope};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimerError {
    #[error("timer context was disposed")]
    ContextDisposed,
    #[error("timer cleanup failed: {0}")]
    Cleanup(String),
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
