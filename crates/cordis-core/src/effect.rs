use crate::{CordisError, EffectId};
use futures::{FutureExt, Stream, StreamExt};
use std::any::Any;
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

type BoxDisposerFuture = Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send + 'static>>;
type DisposerCallback = Box<dyn FnOnce() -> BoxDisposerFuture + Send + 'static>;

/// An asynchronous inverse operation captured by an effect.
pub struct Disposer {
    callback: DisposerCallback,
}

impl Disposer {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), CordisError>> + Send + 'static,
    {
        Self {
            callback: Box::new(move || Box::pin(callback())),
        }
    }

    pub fn infallible<F, Fut>(callback: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self::new(move || async move {
            callback().await;
            Ok(())
        })
    }

    fn run(self) -> BoxDisposerFuture {
        (self.callback)()
    }
}

impl fmt::Debug for Disposer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Disposer(..)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposeErrors {
    errors: Vec<CordisError>,
}

impl DisposeErrors {
    pub fn errors(&self) -> &[CordisError] {
        &self.errors
    }
}

impl fmt::Display for DisposeErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} disposer(s) failed", self.errors.len())
    }
}

impl std::error::Error for DisposeErrors {}

pub type DisposeReport = Result<(), DisposeErrors>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectMeta {
    pub id: EffectId,
    pub label: Arc<str>,
    pub children: Vec<EffectMeta>,
}

#[derive(Clone)]
pub struct EffectGuard {
    inner: Arc<EffectInner>,
}

impl fmt::Debug for EffectGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectGuard")
            .field("id", &self.id())
            .field("label", &self.inner.label)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct EffectScope {
    guard: EffectGuard,
}

/// Owns the top-level effects of one fiber.
#[derive(Clone, Debug)]
pub struct EffectSet {
    root: EffectGuard,
    scope: EffectScope,
}

struct EffectInner {
    id: EffectId,
    label: Arc<str>,
    state: Mutex<EffectState>,
    completed: Notify,
}

struct EffectState {
    status: EffectStatus,
    disposers: Vec<Disposer>,
    children: Vec<EffectGuard>,
    active_runners: usize,
    runner_errors: Vec<CordisError>,
}

enum EffectStatus {
    Armed,
    Draining,
    Disposing,
    Disposed(DisposeReport),
}

impl EffectGuard {
    pub fn new(label: impl Into<Arc<str>>) -> (Self, EffectScope) {
        let guard = Self {
            inner: Arc::new(EffectInner {
                id: EffectId::next(),
                label: label.into(),
                state: Mutex::new(EffectState {
                    status: EffectStatus::Armed,
                    disposers: Vec::new(),
                    children: Vec::new(),
                    active_runners: 0,
                    runner_errors: Vec::new(),
                }),
                completed: Notify::new(),
            }),
        };
        let scope = EffectScope {
            guard: guard.clone(),
        };
        (guard, scope)
    }

    /// Starts an asynchronous effect iterator immediately.
    ///
    /// Each successful stream item is an inverse for the step that just completed.
    /// Disposal stops the iterator only at an item boundary, waits for the in-flight
    /// item, and then runs every collected inverse. A stream error triggers disposal.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn spawn_stream<S>(label: impl Into<Arc<str>>, mut stream: S) -> (Self, EffectScope)
    where
        S: Stream<Item = Result<Disposer, CordisError>> + Send + Unpin + 'static,
    {
        let (guard, scope) = Self::new(label);
        guard.begin_runner();
        let runner_guard = guard.clone();
        tokio::spawn(async move {
            let mut failed = false;
            while runner_guard.runner_should_continue() {
                match AssertUnwindSafe(stream.next()).catch_unwind().await {
                    Ok(Some(Ok(disposer))) => {
                        if !runner_guard.defer_from_runner(disposer) {
                            break;
                        }
                    }
                    Ok(Some(Err(error))) => {
                        runner_guard.record_runner_error(error);
                        failed = true;
                        break;
                    }
                    Ok(None) => break,
                    Err(payload) => {
                        runner_guard.record_runner_error(panic_error(payload.as_ref()));
                        failed = true;
                        break;
                    }
                }
            }
            runner_guard.end_runner();
            if failed {
                let _ = runner_guard.dispose().await;
            }
        });
        (guard, scope)
    }

    pub fn id(&self) -> EffectId {
        self.inner.id
    }

    pub fn is_armed(&self) -> bool {
        matches!(self.lock_state().status, EffectStatus::Armed)
    }

    pub fn is_disposed(&self) -> bool {
        matches!(self.lock_state().status, EffectStatus::Disposed(_))
    }

    pub fn metadata(&self) -> EffectMeta {
        let children = self.lock_state().children.clone();
        EffectMeta {
            id: self.id(),
            label: self.inner.label.clone(),
            children: children
                .iter()
                .filter(|child| !child.is_disposed())
                .map(Self::metadata)
                .collect(),
        }
    }

    /// Runs every inverse in reverse registration order.
    ///
    /// Concurrent and later calls wait for, then return, the same report.
    ///
    /// # Errors
    ///
    /// Returns all disposer failures and panics after every disposer has been attempted.
    pub async fn dispose(&self) -> DisposeReport {
        loop {
            // Register before checking the state so a fast disposer cannot notify
            // between our state check and the waiter becoming visible.
            let completed = self.inner.completed.notified();
            let action = {
                let mut state = self.lock_state();
                match &state.status {
                    EffectStatus::Armed if state.active_runners > 0 => {
                        state.status = EffectStatus::Draining;
                        DisposeAction::Wait
                    }
                    EffectStatus::Armed | EffectStatus::Draining if state.active_runners == 0 => {
                        state.status = EffectStatus::Disposing;
                        DisposeAction::Run {
                            disposers: std::mem::take(&mut state.disposers),
                            errors: std::mem::take(&mut state.runner_errors),
                        }
                    }
                    EffectStatus::Draining | EffectStatus::Disposing => DisposeAction::Wait,
                    EffectStatus::Disposed(report) => DisposeAction::Complete(report.clone()),
                    EffectStatus::Armed => {
                        unreachable!("all armed runner-count cases are handled")
                    }
                }
            };

            match action {
                DisposeAction::Run { disposers, errors } => {
                    let report = run_disposers(disposers, errors).await;
                    self.lock_state().status = EffectStatus::Disposed(report.clone());
                    self.inner.completed.notify_waiters();
                    return report;
                }
                DisposeAction::Wait => completed.await,
                DisposeAction::Complete(report) => return report,
            }
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, EffectState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin_runner(&self) {
        let mut state = self.lock_state();
        assert!(
            matches!(state.status, EffectStatus::Armed),
            "new effect must be armed before its stream starts"
        );
        state.active_runners += 1;
    }

    fn runner_should_continue(&self) -> bool {
        matches!(self.lock_state().status, EffectStatus::Armed)
    }

    fn defer_from_runner(&self, disposer: Disposer) -> bool {
        let mut state = self.lock_state();
        let should_continue = matches!(state.status, EffectStatus::Armed);
        assert!(
            matches!(state.status, EffectStatus::Armed | EffectStatus::Draining),
            "an active stream runner must keep the effect armed or draining"
        );
        state.disposers.push(disposer);
        should_continue
    }

    fn record_runner_error(&self, error: CordisError) {
        self.lock_state().runner_errors.push(error);
    }

    fn end_runner(&self) {
        let mut state = self.lock_state();
        state.active_runners = state
            .active_runners
            .checked_sub(1)
            .expect("effect runner count underflow");
        drop(state);
        self.inner.completed.notify_waiters();
    }
}

impl EffectScope {
    pub fn id(&self) -> EffectId {
        self.guard.id()
    }

    /// Adds an inverse to this effect.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after disposal has started.
    pub fn defer(&self, disposer: Disposer) -> Result<(), CordisError> {
        let mut state = self.guard.lock_state();
        if !matches!(state.status, EffectStatus::Armed) {
            return Err(CordisError::InactiveEffect { effect: self.id() });
        }
        state.disposers.push(disposer);
        Ok(())
    }

    /// Creates a child that is part of this effect's inverse and metadata tree.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after disposal has started.
    pub fn child(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<(EffectGuard, EffectScope), CordisError> {
        let mut state = self.guard.lock_state();
        if !matches!(state.status, EffectStatus::Armed) {
            return Err(CordisError::InactiveEffect { effect: self.id() });
        }

        let (child, scope) = EffectGuard::new(label);
        let child_id = child.id();
        let child_for_dispose = child.clone();
        state.disposers.push(Disposer::new(move || async move {
            child_for_dispose
                .dispose()
                .await
                .map_err(|report| CordisError::ChildEffectFailed {
                    effect: child_id,
                    errors: report.errors,
                })
        }));
        state.children.push(child.clone());
        Ok((child, scope))
    }
}

impl EffectSet {
    pub fn new(label: impl Into<Arc<str>>) -> Self {
        let (root, scope) = EffectGuard::new(label);
        Self { root, scope }
    }

    pub fn id(&self) -> EffectId {
        self.root.id()
    }

    /// Creates a top-level effect owned by this set.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after set disposal has started.
    pub fn effect(
        &self,
        label: impl Into<Arc<str>>,
    ) -> Result<(EffectGuard, EffectScope), CordisError> {
        self.scope.child(label)
    }

    pub fn metadata(&self) -> Vec<EffectMeta> {
        self.root.metadata().children
    }

    /// Disposes every owned effect in reverse creation order.
    ///
    /// Concurrent and later calls return the same report.
    ///
    /// # Errors
    ///
    /// Returns the aggregated child effect failures after all effects were attempted.
    pub async fn dispose(&self) -> DisposeReport {
        self.root.dispose().await
    }
}

enum DisposeAction {
    Run {
        disposers: Vec<Disposer>,
        errors: Vec<CordisError>,
    },
    Wait,
    Complete(DisposeReport),
}

async fn run_disposers(disposers: Vec<Disposer>, mut errors: Vec<CordisError>) -> DisposeReport {
    for disposer in disposers.into_iter().rev() {
        let future = match catch_unwind(AssertUnwindSafe(|| disposer.run())) {
            Ok(future) => future,
            Err(payload) => {
                errors.push(panic_error(payload.as_ref()));
                continue;
            }
        };

        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error),
            Err(payload) => errors.push(panic_error(payload.as_ref())),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(DisposeErrors { errors })
    }
}

fn panic_error(payload: &(dyn Any + Send)) -> CordisError {
    let message = if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    };
    CordisError::DisposerPanic { message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Barrier, Notify};

    async fn wait_for_runner(guard: &EffectGuard) {
        for _ in 0..100 {
            let finished = guard.lock_state().active_runners == 0;
            if finished {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("effect stream did not stop");
    }

    async fn wait_until_disposed(guard: &EffectGuard) {
        for _ in 0..100 {
            let disposed = matches!(guard.lock_state().status, EffectStatus::Disposed(_));
            if disposed {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("effect did not dispose");
    }

    #[tokio::test]
    async fn dispose_is_idempotent() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (guard, scope) = EffectGuard::new("test");
        let calls_for_disposer = calls.clone();
        scope
            .defer(Disposer::infallible(move || async move {
                calls_for_disposer.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap();

        assert!(guard.dispose().await.is_ok());
        assert!(guard.dispose().await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn disposers_run_in_lifo_order() {
        let sequence = Arc::new(Mutex::new(Vec::new()));
        let (guard, scope) = EffectGuard::new("test");
        for value in 1..=3 {
            let sequence = sequence.clone();
            scope
                .defer(Disposer::infallible(move || async move {
                    sequence.lock().unwrap().push(value);
                }))
                .unwrap();
        }

        guard.dispose().await.unwrap();
        assert_eq!(*sequence.lock().unwrap(), vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn concurrent_dispose_waits_for_the_same_cleanup() {
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let (guard, scope) = EffectGuard::new("test");
        let calls_for_disposer = calls.clone();
        let barrier_for_disposer = barrier.clone();
        scope
            .defer(Disposer::infallible(move || async move {
                calls_for_disposer.fetch_add(1, Ordering::SeqCst);
                barrier_for_disposer.wait().await;
            }))
            .unwrap();

        let first_guard = guard.clone();
        let first = tokio::spawn(async move { first_guard.dispose().await });
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let second_guard = guard.clone();
        let second = tokio::spawn(async move { second_guard.dispose().await });

        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        barrier.wait().await;
        assert_eq!(first.await.unwrap(), Ok(()));
        assert_eq!(second.await.unwrap(), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_do_not_skip_remaining_disposers() {
        let sequence = Arc::new(Mutex::new(Vec::new()));
        let (guard, scope) = EffectGuard::new("test");
        let first_sequence = sequence.clone();
        scope
            .defer(Disposer::infallible(move || async move {
                first_sequence.lock().unwrap().push("first");
            }))
            .unwrap();
        scope
            .defer(Disposer::new(|| async {
                Err(CordisError::DisposerFailed {
                    message: "expected".to_owned(),
                })
            }))
            .unwrap();
        let last_sequence = sequence.clone();
        scope
            .defer(Disposer::infallible(move || async move {
                last_sequence.lock().unwrap().push("last");
            }))
            .unwrap();

        let report = guard.dispose().await.unwrap_err();
        assert_eq!(report.errors().len(), 1);
        assert_eq!(*sequence.lock().unwrap(), vec!["last", "first"]);
        assert_eq!(guard.dispose().await, Err(report));
    }

    #[tokio::test]
    async fn disposer_panics_are_reported_and_cleanup_continues() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (guard, scope) = EffectGuard::new("test");
        let calls_after_panic = calls.clone();
        scope
            .defer(Disposer::infallible(move || async move {
                calls_after_panic.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap();
        scope
            .defer(Disposer::new(|| async {
                panic!("expected panic");
                #[allow(unreachable_code)]
                Ok(())
            }))
            .unwrap();

        let report = guard.dispose().await.unwrap_err();
        assert_eq!(report.errors().len(), 1);
        assert!(matches!(
            &report.errors()[0],
            CordisError::DisposerPanic { message } if message == "expected panic"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn inactive_effect_rejects_new_disposers() {
        let (guard, scope) = EffectGuard::new("test");
        guard.dispose().await.unwrap();
        assert_eq!(
            scope.defer(Disposer::infallible(|| async {})),
            Err(CordisError::InactiveEffect { effect: guard.id() })
        );
    }

    #[tokio::test]
    async fn stream_disposal_waits_for_the_in_flight_item() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let started_in_stream = started.clone();
        let release_in_stream = release.clone();
        let calls_in_disposer = calls.clone();
        let stream = stream::once(async move {
            started_in_stream.notify_one();
            release_in_stream.notified().await;
            Ok(Disposer::infallible(move || async move {
                calls_in_disposer.fetch_add(1, Ordering::SeqCst);
            }))
        })
        .boxed();
        let (guard, _) = EffectGuard::spawn_stream("stream", stream);

        started.notified().await;
        let disposing_guard = guard.clone();
        let disposing = tokio::spawn(async move { disposing_guard.dispose().await });
        tokio::task::yield_now().await;
        assert!(!disposing.is_finished());

        release.notify_one();
        disposing.await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_disposers_keep_lifo_order() {
        let sequence = Arc::new(Mutex::new(Vec::new()));
        let items = (1..=3)
            .map(|value| {
                let sequence = sequence.clone();
                Ok(Disposer::infallible(move || async move {
                    sequence.lock().unwrap().push(value);
                }))
            })
            .collect::<Vec<Result<Disposer, CordisError>>>();
        let (guard, _) = EffectGuard::spawn_stream("stream", stream::iter(items));

        wait_for_runner(&guard).await;
        guard.dispose().await.unwrap();
        assert_eq!(*sequence.lock().unwrap(), vec![3, 2, 1]);
    }

    #[tokio::test]
    async fn stream_error_triggers_disposal() {
        let error = CordisError::DisposerFailed {
            message: "stream failed".to_owned(),
        };
        let (guard, _) =
            EffectGuard::spawn_stream("stream", stream::iter(vec![Err(error.clone())]));

        wait_until_disposed(&guard).await;
        let report = guard.dispose().await.unwrap_err();
        assert_eq!(report.errors(), &[error]);
    }

    #[tokio::test]
    async fn nested_effects_are_visible_and_disposed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (parent, parent_scope) = EffectGuard::new("parent");
        let (child, child_scope) = parent_scope.child("child").unwrap();
        let calls_for_child = calls.clone();
        child_scope
            .defer(Disposer::infallible(move || async move {
                calls_for_child.fetch_add(1, Ordering::SeqCst);
            }))
            .unwrap();
        let metadata = parent.metadata();
        assert_eq!(metadata.label.as_ref(), "parent");
        assert_eq!(metadata.children.len(), 1);
        assert_eq!(metadata.children[0].label.as_ref(), "child");

        parent.dispose().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!child.is_armed());
    }

    #[tokio::test]
    async fn effect_set_owns_top_level_effects() {
        let sequence = Arc::new(Mutex::new(Vec::new()));
        let set = EffectSet::new("fiber");
        let (first, first_scope) = set.effect("first").unwrap();
        let (_, second_scope) = set.effect("second").unwrap();
        for (value, scope) in [(1, first_scope), (2, second_scope)] {
            let sequence = sequence.clone();
            scope
                .defer(Disposer::infallible(move || async move {
                    sequence.lock().unwrap().push(value);
                }))
                .unwrap();
        }

        assert_eq!(set.metadata().len(), 2);
        first.dispose().await.unwrap();
        assert_eq!(set.metadata().len(), 1);

        set.dispose().await.unwrap();
        assert_eq!(*sequence.lock().unwrap(), vec![1, 2]);
        assert!(matches!(
            set.effect("late"),
            Err(CordisError::InactiveEffect { .. })
        ));
    }
}
