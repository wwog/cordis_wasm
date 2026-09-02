use crate::{CordisError, Disposer, EffectScope, ListenerId, RealmId};
use futures::FutureExt;
use futures::future::join_all;
use std::any::Any;
use std::fmt;
use std::future::Future;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

type ListenerFuture<B> =
    Pin<Box<dyn Future<Output = Result<ControlFlow<B>, CordisError>> + Send + 'static>>;
type AsyncCallback<P, B> = dyn Fn(P) -> ListenerFuture<B> + Send + Sync + 'static;
type BailCallback<P, B> = dyn Fn(P) -> Result<ControlFlow<B>, CordisError> + Send + Sync + 'static;
type WaterfallFuture<T> = Pin<Box<dyn Future<Output = Result<T, CordisError>> + Send + 'static>>;
type WaterfallCallback<T> = dyn Fn(T, Next<T>) -> WaterfallFuture<T> + Send + Sync + 'static;

/// Realm used to select listeners for one dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventTarget {
    Global,
    Realm(RealmId),
}

/// Registration-time listener selection and ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListenerOptions {
    pub target: EventTarget,
    pub prepend: bool,
}

impl ListenerOptions {
    pub const fn global() -> Self {
        Self {
            target: EventTarget::Global,
            prepend: false,
        }
    }

    pub const fn realm(realm: RealmId) -> Self {
        Self {
            target: EventTarget::Realm(realm),
            prepend: false,
        }
    }

    #[must_use]
    pub const fn prepend(mut self) -> Self {
        self.prepend = true;
        self
    }
}

impl Default for ListenerOptions {
    fn default() -> Self {
        Self::global()
    }
}

/// Effect-owned asynchronous listener collection.
#[derive(Clone)]
pub struct AsyncEvent<P: 'static, B: 'static> {
    listeners: ListenerStore<AsyncCallback<P, B>>,
}

impl<P, B> fmt::Debug for AsyncEvent<P, B> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("AsyncEvent").finish_non_exhaustive()
    }
}

impl<P: 'static, B: 'static> Default for AsyncEvent<P, B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: 'static, B: 'static> AsyncEvent<P, B> {
    pub fn new() -> Self {
        Self {
            listeners: ListenerStore::new(),
        }
    }

    /// Registers a listener whose lifetime is owned by `effect`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if disposal has started.
    pub fn listen<F, Fut>(
        &self,
        effect: &EffectScope,
        options: ListenerOptions,
        callback: F,
    ) -> Result<ListenerId, CordisError>
    where
        F: Fn(P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ControlFlow<B>, CordisError>> + Send + 'static,
    {
        self.listeners.register(
            effect,
            options,
            Arc::new(move |payload| Box::pin(callback(payload))),
        )
    }
}

impl<P, B> AsyncEvent<P, B>
where
    P: Clone + Send + 'static,
    B: Send + 'static,
{
    /// Runs all matching listeners concurrently and preserves registration order.
    ///
    /// # Errors
    ///
    /// Aggregates every listener error after all listeners finish.
    pub async fn parallel(
        &self,
        target: EventTarget,
        payload: &P,
    ) -> Result<Vec<ControlFlow<B>>, CordisError> {
        let calls = self
            .listeners
            .matching(target)
            .into_iter()
            .map(|callback| run_async(callback, payload.clone()));
        let mut values = Vec::new();
        let mut errors = Vec::new();
        for result in join_all(calls).await {
            match result {
                Ok(value) => values.push(value),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(values)
        } else {
            Err(CordisError::EventListenersFailed { errors })
        }
    }

    /// Runs listeners in order and stops at the first `Break`.
    ///
    /// # Errors
    ///
    /// Returns the first listener error or panic.
    pub async fn serial(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError> {
        for callback in self.listeners.matching(target) {
            if let ControlFlow::Break(value) = run_async(callback, payload.clone()).await? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Starts matching listeners in order without waiting for completion.
    ///
    /// Asynchronous failures and panics are delivered to `error_sink`.
    ///
    /// # Errors
    ///
    /// Returns immediately if invoking a listener panics before producing its future.
    /// Listeners already started remain running.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn emit_nowait<S>(
        &self,
        target: EventTarget,
        payload: &P,
        error_sink: S,
    ) -> Result<(), CordisError>
    where
        S: Fn(CordisError) + Send + Sync + 'static,
    {
        let error_sink = Arc::new(error_sink);
        for callback in self.listeners.matching(target) {
            let future = catch_unwind(AssertUnwindSafe(|| callback(payload.clone())))
                .map_err(|panic| listener_panic(panic.as_ref()))?;
            let error_sink = Arc::clone(&error_sink);
            tokio::spawn(async move {
                match AssertUnwindSafe(future).catch_unwind().await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => error_sink(error),
                    Err(panic) => error_sink(listener_panic(panic.as_ref())),
                }
            });
        }
        Ok(())
    }
}

/// One-shot continuation passed to a waterfall listener.
pub struct Next<T: Send + 'static> {
    step: Option<WaterfallStep<T>>,
}

impl<T: Send + 'static> fmt::Debug for Next<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Next")
            .field("available", &self.step.is_some())
            .finish()
    }
}

impl<T: Send + 'static> Next<T> {
    /// Invokes the rest of the waterfall exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::NextAlreadyUsed`] on a second call, or propagates a
    /// downstream listener error.
    pub async fn call(&mut self, value: T) -> Result<T, CordisError> {
        let step = self.step.take().ok_or(CordisError::NextAlreadyUsed)?;
        run_waterfall(step, value).await
    }
}

struct WaterfallStep<T: Send + 'static> {
    callbacks: Arc<[Arc<WaterfallCallback<T>>]>,
    index: usize,
}

/// Effect-owned onion middleware event.
#[derive(Clone)]
pub struct WaterfallEvent<T: Send + 'static> {
    listeners: ListenerStore<WaterfallCallback<T>>,
}

impl<T: Send + 'static> fmt::Debug for WaterfallEvent<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WaterfallEvent")
            .finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Default for WaterfallEvent<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + 'static> WaterfallEvent<T> {
    pub fn new() -> Self {
        Self {
            listeners: ListenerStore::new(),
        }
    }

    /// Registers one onion middleware whose lifetime is owned by `effect`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if disposal has started.
    pub fn listen<F, Fut>(
        &self,
        effect: &EffectScope,
        options: ListenerOptions,
        callback: F,
    ) -> Result<ListenerId, CordisError>
    where
        F: Fn(T, Next<T>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, CordisError>> + Send + 'static,
    {
        self.listeners.register(
            effect,
            options,
            Arc::new(move |value, next| Box::pin(callback(value, next))),
        )
    }

    /// Runs matching middleware as an onion chain.
    ///
    /// # Errors
    ///
    /// Propagates listener errors and converts panics to
    /// [`CordisError::EventListenerPanicked`].
    pub async fn run(&self, target: EventTarget, value: T) -> Result<T, CordisError> {
        let callbacks = Arc::from(self.listeners.matching(target));
        run_waterfall(
            WaterfallStep {
                callbacks,
                index: 0,
            },
            value,
        )
        .await
    }
}

fn run_waterfall<T: Send + 'static>(step: WaterfallStep<T>, value: T) -> WaterfallFuture<T> {
    Box::pin(async move {
        let Some(callback) = step.callbacks.get(step.index).cloned() else {
            return Ok(value);
        };
        let next = Next {
            step: Some(WaterfallStep {
                callbacks: step.callbacks,
                index: step.index + 1,
            }),
        };
        let future = catch_unwind(AssertUnwindSafe(|| callback(value, next)))
            .map_err(|panic| listener_panic(panic.as_ref()))?;
        AssertUnwindSafe(future)
            .catch_unwind()
            .await
            .map_err(|panic| listener_panic(panic.as_ref()))?
    })
}

/// Effect-owned synchronous event for deterministic bail dispatch.
#[derive(Clone)]
pub struct BailEvent<P: 'static, B: 'static> {
    listeners: ListenerStore<BailCallback<P, B>>,
}

impl<P, B> fmt::Debug for BailEvent<P, B> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("BailEvent").finish_non_exhaustive()
    }
}

impl<P: 'static, B: 'static> Default for BailEvent<P, B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: 'static, B: 'static> BailEvent<P, B> {
    pub fn new() -> Self {
        Self {
            listeners: ListenerStore::new(),
        }
    }

    /// Registers a synchronous listener whose lifetime is owned by `effect`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] if disposal has started.
    pub fn listen<F>(
        &self,
        effect: &EffectScope,
        options: ListenerOptions,
        callback: F,
    ) -> Result<ListenerId, CordisError>
    where
        F: Fn(P) -> Result<ControlFlow<B>, CordisError> + Send + Sync + 'static,
    {
        self.listeners.register(effect, options, Arc::new(callback))
    }
}

impl<P, B: 'static> BailEvent<P, B>
where
    P: Clone + 'static,
{
    /// Runs synchronous listeners in order and stops at the first `Break`.
    ///
    /// # Errors
    ///
    /// Returns the first listener error or panic.
    pub fn bail(&self, target: EventTarget, payload: &P) -> Result<Option<B>, CordisError> {
        for callback in self.listeners.matching(target) {
            let result = catch_unwind(AssertUnwindSafe(|| callback(payload.clone())))
                .map_err(|panic| listener_panic(panic.as_ref()))??;
            if let ControlFlow::Break(value) = result {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }
}

struct ListenerEntry<C: ?Sized + Send + Sync + 'static> {
    id: ListenerId,
    options: ListenerOptions,
    callback: Arc<C>,
}

struct ListenerStore<C: ?Sized + Send + Sync + 'static> {
    entries: Arc<Mutex<Vec<ListenerEntry<C>>>>,
}

impl<C: ?Sized + Send + Sync + 'static> Clone for ListenerStore<C> {
    fn clone(&self) -> Self {
        Self {
            entries: Arc::clone(&self.entries),
        }
    }
}

impl<C: ?Sized + Send + Sync + 'static> ListenerStore<C> {
    fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn register(
        &self,
        effect: &EffectScope,
        options: ListenerOptions,
        callback: Arc<C>,
    ) -> Result<ListenerId, CordisError> {
        let id = ListenerId::next();
        self.lock().push(ListenerEntry {
            id,
            options,
            callback,
        });

        let listeners = self.clone();
        if let Err(error) = effect.defer(Disposer::infallible(move || async move {
            listeners.remove(id);
        })) {
            self.remove(id);
            return Err(error);
        }
        Ok(id)
    }

    fn remove(&self, id: ListenerId) {
        self.lock().retain(|entry| entry.id != id);
    }

    fn matching(&self, target: EventTarget) -> Vec<Arc<C>> {
        let mut entries = self
            .lock()
            .iter()
            .filter(|entry| listener_matches(entry.options.target, target))
            .map(|entry| (entry.options.prepend, entry.id, Arc::clone(&entry.callback)))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(prepend, id, _)| (!*prepend, *id));
        entries
            .into_iter()
            .map(|(_, _, callback)| callback)
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ListenerEntry<C>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn listener_matches(listener: EventTarget, dispatch: EventTarget) -> bool {
    listener == EventTarget::Global || listener == dispatch
}

async fn run_async<P, B>(
    callback: Arc<AsyncCallback<P, B>>,
    payload: P,
) -> Result<ControlFlow<B>, CordisError> {
    let future = catch_unwind(AssertUnwindSafe(|| callback(payload)))
        .map_err(|panic| listener_panic(panic.as_ref()))?;
    AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .map_err(|panic| listener_panic(panic.as_ref()))?
}

fn listener_panic(payload: &(dyn Any + Send)) -> CordisError {
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    CordisError::EventListenerPanicked { message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectGuard;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Barrier, Notify};

    #[tokio::test]
    async fn listeners_are_effect_owned_and_prepend_has_its_own_segment() {
        let event = AsyncEvent::<(), ()>::new();
        let (effect, scope) = EffectGuard::new("listeners");
        let calls = Arc::new(Mutex::new(Vec::new()));
        for (value, options) in [
            (1, ListenerOptions::global()),
            (2, ListenerOptions::global().prepend()),
            (3, ListenerOptions::global().prepend()),
        ] {
            let calls = Arc::clone(&calls);
            event
                .listen(&scope, options, move |()| {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.lock().unwrap().push(value);
                        Ok(ControlFlow::Continue(()))
                    }
                })
                .unwrap();
        }

        event.serial(EventTarget::Global, &()).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![2, 3, 1]);
        effect.dispose().await.unwrap();
        calls.lock().unwrap().clear();
        event.serial(EventTarget::Global, &()).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parallel_starts_all_listeners_before_waiting() {
        let event = AsyncEvent::<(), ()>::new();
        let (_effect, scope) = EffectGuard::new("parallel");
        let barrier = Arc::new(Barrier::new(2));
        for _ in 0..2 {
            let barrier = Arc::clone(&barrier);
            event
                .listen(&scope, ListenerOptions::global(), move |()| {
                    let barrier = Arc::clone(&barrier);
                    async move {
                        barrier.wait().await;
                        Ok(ControlFlow::Continue(()))
                    }
                })
                .unwrap();
        }
        assert_eq!(
            event
                .parallel(EventTarget::Global, &())
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn serial_breaks_and_realm_filter_keeps_global_listeners() {
        let event = AsyncEvent::<(), &'static str>::new();
        let (_effect, scope) = EffectGuard::new("serial");
        let realm = RealmId::next();
        let other = RealmId::next();
        let calls = Arc::new(AtomicUsize::new(0));
        for (options, result) in [
            (ListenerOptions::global(), ControlFlow::Continue(())),
            (ListenerOptions::realm(other), ControlFlow::Break("wrong")),
            (ListenerOptions::realm(realm), ControlFlow::Break("stop")),
        ] {
            let calls = Arc::clone(&calls);
            event
                .listen(&scope, options, move |()| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async move { Ok(result) }
                })
                .unwrap();
        }
        assert_eq!(
            event.serial(EventTarget::Realm(realm), &()).await.unwrap(),
            Some("stop")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn emit_nowait_reports_asynchronous_errors() {
        let event = AsyncEvent::<(), ()>::new();
        let (_effect, scope) = EffectGuard::new("nowait");
        event
            .listen(&scope, ListenerOptions::global(), |()| async {
                Err(CordisError::DisposerFailed {
                    message: "listener failed".to_owned(),
                })
            })
            .unwrap();
        let error = Arc::new(Mutex::new(None));
        let notified = Arc::new(Notify::new());
        let captured = Arc::clone(&error);
        let signal = Arc::clone(&notified);
        event
            .emit_nowait(EventTarget::Global, &(), move |failure| {
                *captured.lock().unwrap() = Some(failure);
                signal.notify_one();
            })
            .unwrap();
        notified.notified().await;
        assert!(matches!(
            *error.lock().unwrap(),
            Some(CordisError::DisposerFailed { .. })
        ));
    }

    #[tokio::test]
    async fn waterfall_runs_as_an_onion() {
        let event = WaterfallEvent::<i32>::new();
        let (_effect, scope) = EffectGuard::new("waterfall");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let outer_calls = Arc::clone(&calls);
        event
            .listen(&scope, ListenerOptions::global(), move |value, mut next| {
                let calls = Arc::clone(&outer_calls);
                async move {
                    calls.lock().unwrap().push("outer-before");
                    let value = next.call(value + 1).await?;
                    calls.lock().unwrap().push("outer-after");
                    Ok(value + 1)
                }
            })
            .unwrap();
        let inner_calls = Arc::clone(&calls);
        event
            .listen(&scope, ListenerOptions::global(), move |value, mut next| {
                let calls = Arc::clone(&inner_calls);
                async move {
                    calls.lock().unwrap().push("inner-before");
                    let value = next.call(value + 10).await?;
                    calls.lock().unwrap().push("inner-after");
                    Ok(value + 10)
                }
            })
            .unwrap();

        assert_eq!(event.run(EventTarget::Global, 0).await.unwrap(), 22);
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["outer-before", "inner-before", "inner-after", "outer-after"]
        );
    }

    #[tokio::test]
    async fn waterfall_next_rejects_a_second_call() {
        let event = WaterfallEvent::<i32>::new();
        let (_effect, scope) = EffectGuard::new("waterfall");
        event
            .listen(
                &scope,
                ListenerOptions::global(),
                |value, mut next| async move {
                    next.call(value).await?;
                    next.call(value).await
                },
            )
            .unwrap();
        assert_eq!(
            event.run(EventTarget::Global, 1).await,
            Err(CordisError::NextAlreadyUsed)
        );
    }

    #[test]
    fn bail_is_synchronous_and_stops_at_break() {
        let event = BailEvent::<(), &'static str>::new();
        let (_effect, scope) = EffectGuard::new("bail");
        let calls = Arc::new(AtomicUsize::new(0));
        for result in [ControlFlow::Continue(()), ControlFlow::Break("stop")] {
            let calls = Arc::clone(&calls);
            event
                .listen(&scope, ListenerOptions::global(), move |()| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(result)
                })
                .unwrap();
        }
        assert_eq!(event.bail(EventTarget::Global, &()).unwrap(), Some("stop"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
