use crate::{
    CommittedView, Context, CordisError, DependencyResolution, DesiredEpoch, DesiredState, FiberId,
    FiberMachine, FiberState, FiberTransition, InjectSpec, ProviderKey, RealmId, RegistryChange,
    ResolvedInject, TransitionAdvance,
};
use futures::FutureExt;
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

const COMMAND_BUFFER: usize = 64;

/// Owns the single-writer supervisor task.
#[derive(Debug)]
pub struct Runtime {
    handle: RuntimeHandle,
    supervisor: JoinHandle<()>,
}

/// Cloneable command handle for the runtime supervisor.
#[derive(Clone)]
pub struct RuntimeHandle {
    commands: mpsc::Sender<Command>,
    executors: Arc<RwLock<BTreeMap<FiberId, FiberExecutor>>>,
    changes: Arc<Notify>,
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let executor_count = self
            .executors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("RuntimeHandle")
            .field("executor_count", &executor_count)
            .finish_non_exhaustive()
    }
}

pub(crate) type FiberWork = Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send + 'static>>;
pub(crate) type FiberExecutor = Arc<dyn Fn(FiberTransition) -> FiberWork + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub desired: DependencyResolution,
    pub committed: Option<CommittedView>,
    pub state: FiberState,
    pub active_transition: Option<FiberTransition>,
    pub dependency_error: Option<CordisError>,
    pub failure: Option<CordisError>,
    pub teardown_error: Option<CordisError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub fibers: Vec<FiberSnapshot>,
    pub allocated_realms: u64,
    pub provider_count: usize,
}

/// Dependency configuration result and any transition it made runnable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyChange {
    pub resolution: DependencyResolution,
    pub transitions: Vec<FiberTransition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionStatus {
    Applied,
    IgnoredStale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionUpdate {
    pub status: CompletionStatus,
    pub ready: Vec<FiberTransition>,
}

#[derive(Debug)]
struct FiberRecord {
    snapshot: FiberSnapshot,
    context: Option<Context>,
    injects: Vec<InjectSpec>,
    machine: FiberMachine,
    retired: bool,
}

#[derive(Debug)]
enum Command {
    CreateFiber {
        parent: Option<FiberId>,
        require_live_parent: bool,
        reply: oneshot::Sender<Result<FiberId, CordisError>>,
    },
    AllocateRealm {
        reply: oneshot::Sender<RealmId>,
    },
    ConfigureDependencies {
        fiber: FiberId,
        context: Context,
        injects: Vec<InjectSpec>,
        reply: oneshot::Sender<Result<DependencyChange, CordisError>>,
    },
    CommitDependencies {
        fiber: FiberId,
        reply: oneshot::Sender<Result<CommittedView, CordisError>>,
    },
    Provide {
        key: ProviderKey,
        provider: FiberId,
        reply: oneshot::Sender<Result<RegistryChange, CordisError>>,
    },
    Withdraw {
        key: ProviderKey,
        provider: FiberId,
        reply: oneshot::Sender<Result<RegistryChange, CordisError>>,
    },
    CompleteTransition {
        fiber: FiberId,
        generation: u64,
        result: Result<(), CordisError>,
        reply: oneshot::Sender<Result<TransitionUpdate, CordisError>>,
    },
    RetireFiber {
        fiber: FiberId,
        reply: oneshot::Sender<Result<Vec<FiberTransition>, CordisError>>,
    },
    RestartFiber {
        fiber: FiberId,
        reply: oneshot::Sender<Result<Vec<FiberTransition>, CordisError>>,
    },
    ReloadFiber {
        fiber: FiberId,
        reply: oneshot::Sender<Result<Vec<FiberTransition>, CordisError>>,
    },
    Snapshot {
        reply: oneshot::Sender<RuntimeSnapshot>,
    },
    Shutdown {
        reply: oneshot::Sender<RuntimeSnapshot>,
    },
}

#[derive(Debug, Default)]
struct SupervisorState {
    fibers: BTreeMap<FiberId, FiberRecord>,
    providers: BTreeMap<ProviderKey, FiberId>,
    allocated_realms: u64,
    blocked_unloads: BTreeMap<FiberId, FiberTransition>,
}

impl Runtime {
    /// Starts the single-writer supervisor.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub fn start() -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_BUFFER);
        let supervisor = tokio::spawn(run_supervisor(receiver));
        let executors = Arc::new(RwLock::new(BTreeMap::new()));
        let changes = Arc::new(Notify::new());
        Self {
            handle: RuntimeHandle {
                commands,
                executors,
                changes,
            },
            supervisor,
        }
    }

    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    /// Stops the supervisor after all earlier commands and returns its final snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the supervisor closed or its task failed.
    pub async fn shutdown(self) -> Result<RuntimeSnapshot, CordisError> {
        let snapshot = self.handle.request_shutdown().await?;
        self.supervisor
            .await
            .map_err(|error| CordisError::SupervisorFailed {
                message: error.to_string(),
            })?;
        Ok(snapshot)
    }
}

impl RuntimeHandle {
    /// Creates a fiber after validating its optional parent.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::UnknownFiber`] for an unknown parent, or
    /// [`CordisError::RuntimeClosed`] after shutdown.
    pub async fn create_fiber(&self, parent: Option<FiberId>) -> Result<FiberId, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::CreateFiber {
            parent,
            require_live_parent: false,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        result
    }

    /// Creates a child fiber while its parent is loading or active.
    ///
    /// This is the lifecycle-safe entry point used by generated method-level injects.
    pub(crate) async fn create_live_child_fiber(
        &self,
        parent: FiberId,
    ) -> Result<FiberId, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::CreateFiber {
            parent: Some(parent),
            require_live_parent: true,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        result
    }

    /// Allocates a realm ID that will never be reused by this process.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::RuntimeClosed`] after shutdown.
    pub async fn allocate_realm(&self) -> Result<RealmId, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::AllocateRealm { reply }).await?;
        response.await.map_err(|_| CordisError::RuntimeClosed)
    }

    /// Declares or replaces a fiber's dependencies and computes its desired resolution.
    /// An explicit replacement also retries a failed fiber.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber, a mismatched context, duplicate injects,
    /// a missing realm, or a closed runtime.
    pub async fn configure_dependencies(
        &self,
        fiber: FiberId,
        context: Context,
        injects: Vec<InjectSpec>,
    ) -> Result<DependencyChange, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ConfigureDependencies {
            fiber,
            context,
            injects,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(change) = &result {
            self.dispatch(change.transitions.clone());
        }
        result
    }

    /// Freezes the current ready dependency resolution for one load epoch.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber, an inactive dependency, or a closed runtime.
    pub async fn commit_dependencies(&self, fiber: FiberId) -> Result<CommittedView, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::CommitDependencies { fiber, reply })
            .await?;
        response.await.map_err(|_| CordisError::RuntimeClosed)?
    }

    /// Occupies one `(service, realm)` provider slot.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown provider fiber, an occupied slot, or a closed runtime.
    pub async fn provide(
        &self,
        key: ProviderKey,
        provider: FiberId,
    ) -> Result<RegistryChange, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Provide {
            key,
            provider,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(change) = &result {
            self.dispatch(change.transitions.clone());
        }
        result
    }

    /// Releases one provider slot owned by `provider`.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing slot, wrong owner, or closed runtime.
    pub async fn withdraw(
        &self,
        key: ProviderKey,
        provider: FiberId,
    ) -> Result<RegistryChange, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Withdraw {
            key,
            provider,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(change) = &result {
            self.dispatch(change.transitions.clone());
        }
        result
    }

    /// Reports work completion without running user code inside the supervisor.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber or a closed runtime.
    pub async fn complete_transition(
        &self,
        fiber: FiberId,
        generation: u64,
        result: Result<(), CordisError>,
    ) -> Result<TransitionUpdate, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::CompleteTransition {
            fiber,
            generation,
            result,
            reply,
        })
        .await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(update) = &result {
            self.dispatch(update.ready.clone());
        }
        result
    }

    /// Marks a fiber retired and returns cleanup work when required.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber or a closed runtime.
    pub async fn retire_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::RetireFiber { fiber, reply }).await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(transitions) = &result {
            self.dispatch(transitions.clone());
        }
        result
    }

    /// Explicitly retries a failed fiber against its latest desired epoch.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber or a closed runtime.
    pub async fn restart_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::RestartFiber { fiber, reply }).await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(transitions) = &result {
            self.dispatch(transitions.clone());
        }
        result
    }

    /// Forces an active fiber through unload and a fresh load of its desired epoch.
    ///
    /// Unlike [`Self::restart_fiber`], this does not retry a failed fiber.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown fiber or a closed runtime.
    pub async fn reload_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::ReloadFiber { fiber, reply }).await?;
        let result = response.await.map_err(|_| CordisError::RuntimeClosed)?;
        self.changes.notify_waiters();
        if let Ok(transitions) = &result {
            self.dispatch(transitions.clone());
        }
        result
    }

    /// Returns a stable snapshot produced by the supervisor.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::RuntimeClosed`] after shutdown.
    pub async fn snapshot(&self) -> Result<RuntimeSnapshot, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Snapshot { reply }).await?;
        response.await.map_err(|_| CordisError::RuntimeClosed)
    }

    /// Waits until no Fiber transition is in flight and returns a stable snapshot.
    /// Fibers waiting for missing dependencies are considered quiescent.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::RuntimeClosed`] after shutdown.
    pub async fn await_quiescent(&self) -> Result<RuntimeSnapshot, CordisError> {
        loop {
            let changed = self.changes.notified();
            let snapshot = self.snapshot().await?;
            if snapshot
                .fibers
                .iter()
                .all(|fiber| fiber.active_transition.is_none())
            {
                return Ok(snapshot);
            }
            changed.await;
        }
    }

    async fn request_shutdown(&self) -> Result<RuntimeSnapshot, CordisError> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Shutdown { reply }).await?;
        response.await.map_err(|_| CordisError::RuntimeClosed)
    }

    async fn send(&self, command: Command) -> Result<(), CordisError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| CordisError::RuntimeClosed)
    }

    pub(crate) fn install_executor(&self, fiber: FiberId, executor: FiberExecutor) {
        self.executors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(fiber, executor);
    }

    pub(crate) fn remove_executor(&self, fiber: FiberId) {
        self.executors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&fiber);
    }

    pub(crate) async fn await_disposed(&self, fiber: FiberId) -> Result<(), CordisError> {
        loop {
            let changed = self.changes.notified();
            let snapshot = self.snapshot().await?;
            let state = snapshot
                .fibers
                .iter()
                .find(|candidate| candidate.id == fiber)
                .ok_or(CordisError::UnknownFiber { fiber })?
                .state;
            if state == FiberState::Disposed {
                return Ok(());
            }
            changed.await;
        }
    }

    pub(crate) async fn await_settled(&self, fiber: FiberId) -> Result<FiberSnapshot, CordisError> {
        loop {
            let changed = self.changes.notified();
            let snapshot = self.snapshot().await?;
            let fiber = snapshot
                .fibers
                .into_iter()
                .find(|candidate| candidate.id == fiber)
                .ok_or(CordisError::UnknownFiber { fiber })?;
            if fiber.active_transition.is_none() {
                return Ok(fiber);
            }
            changed.await;
        }
    }

    fn dispatch(&self, transitions: Vec<FiberTransition>) {
        for transition in transitions {
            let executor = self
                .executors
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&transition.fiber)
                .cloned();
            let Some(executor) = executor else {
                continue;
            };
            let handle = self.clone();
            tokio::spawn(async move {
                let work = catch_unwind(AssertUnwindSafe(|| executor(transition.clone())));
                let result = match work {
                    Ok(work) => match AssertUnwindSafe(work).catch_unwind().await {
                        Ok(result) => result,
                        Err(payload) => Err(CordisError::FiberExecutorPanicked {
                            fiber: transition.fiber,
                            message: panic_message(payload.as_ref()),
                        }),
                    },
                    Err(payload) => Err(CordisError::FiberExecutorPanicked {
                        fiber: transition.fiber,
                        message: panic_message(payload.as_ref()),
                    }),
                };
                let _ = handle
                    .complete_transition(transition.fiber, transition.generation, result)
                    .await;
            });
        }
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

async fn run_supervisor(mut commands: mpsc::Receiver<Command>) {
    let mut state = SupervisorState::default();
    while let Some(command) = commands.recv().await {
        match command {
            Command::CreateFiber {
                parent,
                require_live_parent,
                reply,
            } => {
                let result = create_fiber(&mut state, parent, require_live_parent);
                let _ = reply.send(result);
            }
            Command::AllocateRealm { reply } => {
                let realm = RealmId::next();
                state.allocated_realms += 1;
                let _ = reply.send(realm);
            }
            Command::ConfigureDependencies {
                fiber,
                context,
                injects,
                reply,
            } => {
                let result = configure_dependencies(&mut state, fiber, context, injects);
                let _ = reply.send(result);
            }
            Command::CommitDependencies { fiber, reply } => {
                let result = commit_dependencies(&mut state, fiber);
                let _ = reply.send(result);
            }
            Command::Provide {
                key,
                provider,
                reply,
            } => {
                let result = provide(&mut state, key, provider);
                let _ = reply.send(result);
            }
            Command::Withdraw {
                key,
                provider,
                reply,
            } => {
                let result = withdraw(&mut state, key, provider);
                let _ = reply.send(result);
            }
            Command::CompleteTransition {
                fiber,
                generation,
                result,
                reply,
            } => {
                let result = complete_transition(&mut state, fiber, generation, result);
                let _ = reply.send(result);
            }
            Command::RetireFiber { fiber, reply } => {
                let result = retire_fiber(&mut state, fiber);
                let _ = reply.send(result);
            }
            Command::RestartFiber { fiber, reply } => {
                let result = restart_fiber(&mut state, fiber);
                let _ = reply.send(result);
            }
            Command::ReloadFiber { fiber, reply } => {
                let result = reload_fiber(&mut state, fiber);
                let _ = reply.send(result);
            }
            Command::Snapshot { reply } => {
                let _ = reply.send(snapshot(&state));
            }
            Command::Shutdown { reply } => {
                let _ = reply.send(snapshot(&state));
                break;
            }
        }
    }
}

fn create_fiber(
    state: &mut SupervisorState,
    parent: Option<FiberId>,
    require_live_parent: bool,
) -> Result<FiberId, CordisError> {
    if let Some(parent) = parent {
        let parent_record = state
            .fibers
            .get(&parent)
            .ok_or(CordisError::UnknownFiber { fiber: parent })?;
        if require_live_parent
            && !matches!(
                parent_record.machine.state(),
                FiberState::Loading | FiberState::Active
            )
        {
            return Err(CordisError::InactiveFiber { fiber: parent });
        }
    }

    let id = FiberId::next();
    state.fibers.insert(
        id,
        FiberRecord {
            snapshot: FiberSnapshot {
                id,
                parent,
                desired: DependencyResolution::default(),
                committed: None,
                state: FiberState::Pending,
                active_transition: None,
                dependency_error: None,
                failure: None,
                teardown_error: None,
            },
            context: None,
            injects: Vec::new(),
            machine: FiberMachine::new(id),
            retired: false,
        },
    );
    Ok(id)
}

fn configure_dependencies(
    state: &mut SupervisorState,
    fiber: FiberId,
    context: Context,
    injects: Vec<InjectSpec>,
) -> Result<DependencyChange, CordisError> {
    if context.fiber() != fiber {
        return Err(CordisError::ContextFiberMismatch {
            expected: fiber,
            actual: context.fiber(),
        });
    }
    if !state.fibers.contains_key(&fiber) {
        return Err(CordisError::UnknownFiber { fiber });
    }
    validate_injects(fiber, &injects)?;
    let desired = resolve_dependencies(state, &context, &injects)?;

    let was_failed = {
        let record = state
            .fibers
            .get_mut(&fiber)
            .expect("fiber was validated above");
        let was_failed = record.machine.state() == FiberState::Failed;
        record.context = Some(context);
        record.injects = injects;
        record.snapshot.desired = desired.clone();
        was_failed
    };
    let mut transitions = reconcile_lifecycles(state);
    if was_failed
        && let Some(transition) = state
            .fibers
            .get_mut(&fiber)
            .expect("fiber was validated above")
            .machine
            .restart()
    {
        transitions.push(transition);
    }
    let transitions = schedule_transition_batch(state, transitions);
    Ok(DependencyChange {
        resolution: desired,
        transitions,
    })
}

fn commit_dependencies(
    state: &mut SupervisorState,
    fiber: FiberId,
) -> Result<CommittedView, CordisError> {
    let record = state
        .fibers
        .get_mut(&fiber)
        .ok_or(CordisError::UnknownFiber { fiber })?;
    let committed = record.snapshot.desired.commit()?;
    record.snapshot.committed = Some(committed.clone());
    Ok(committed)
}

fn provide(
    state: &mut SupervisorState,
    key: ProviderKey,
    provider: FiberId,
) -> Result<RegistryChange, CordisError> {
    if !state.fibers.contains_key(&provider) {
        return Err(CordisError::UnknownFiber { fiber: provider });
    }
    if let Some(existing) = state.providers.get(&key) {
        return Err(CordisError::DuplicateProvider {
            key,
            provider: *existing,
        });
    }
    state.providers.insert(key.clone(), provider);
    let affected = recompute_affected(state, &key);
    let transitions = reconcile_lifecycles(state);
    let transitions = schedule_transition_batch(state, transitions);
    Ok(RegistryChange {
        key,
        affected,
        transitions,
    })
}

fn withdraw(
    state: &mut SupervisorState,
    key: ProviderKey,
    provider: FiberId,
) -> Result<RegistryChange, CordisError> {
    let actual = state
        .providers
        .get(&key)
        .copied()
        .ok_or_else(|| CordisError::ProviderNotFound { key: key.clone() })?;
    if actual != provider {
        return Err(CordisError::ProviderOwnershipMismatch {
            key,
            expected: provider,
            actual,
        });
    }
    state.providers.remove(&key);
    let affected = recompute_affected(state, &key);
    let transitions = reconcile_lifecycles(state);
    let transitions = schedule_transition_batch(state, transitions);
    Ok(RegistryChange {
        key,
        affected,
        transitions,
    })
}

fn complete_transition(
    state: &mut SupervisorState,
    fiber: FiberId,
    generation: u64,
    result: Result<(), CordisError>,
) -> Result<TransitionUpdate, CordisError> {
    if state.blocked_unloads.contains_key(&fiber) {
        return Err(CordisError::TransitionBlocked { fiber });
    }
    let failed = result.is_err();
    let (completed_kind, advance) = {
        let record = state
            .fibers
            .get_mut(&fiber)
            .ok_or(CordisError::UnknownFiber { fiber })?;
        let completed_kind = record
            .machine
            .active_transition()
            .filter(|transition| transition.generation == generation)
            .map(|transition| transition.kind.clone());
        let advance = record.machine.complete(generation, result);
        (completed_kind, advance)
    };
    let failed_load = failed && matches!(&completed_kind, Some(crate::TransitionKind::Load { .. }));
    if matches!(&completed_kind, Some(crate::TransitionKind::Unload)) || failed_load {
        state
            .fibers
            .get_mut(&fiber)
            .expect("fiber was validated above")
            .snapshot
            .committed = None;
    }

    let (status, transition) = match advance {
        TransitionAdvance::IgnoredStale => (CompletionStatus::IgnoredStale, None),
        TransitionAdvance::Settled => (CompletionStatus::Applied, None),
        TransitionAdvance::Start(transition) => (CompletionStatus::Applied, Some(transition)),
    };
    let mut transitions = transition.into_iter().collect::<Vec<_>>();
    if failed_load {
        transitions.extend(withdraw_provider_bindings(state, fiber));
    }
    let ready = schedule_transition_batch(state, transitions);
    Ok(TransitionUpdate { status, ready })
}

fn retire_fiber(
    state: &mut SupervisorState,
    fiber: FiberId,
) -> Result<Vec<FiberTransition>, CordisError> {
    let record = state
        .fibers
        .get_mut(&fiber)
        .ok_or(CordisError::UnknownFiber { fiber })?;
    record.retired = true;
    let transition = record.machine.set_desired(DesiredState::Retired);
    let mut transitions = withdraw_provider_bindings(state, fiber);
    transitions.extend(transition);
    Ok(schedule_transition_batch(state, transitions))
}

fn restart_fiber(
    state: &mut SupervisorState,
    fiber: FiberId,
) -> Result<Vec<FiberTransition>, CordisError> {
    let transition = state
        .fibers
        .get_mut(&fiber)
        .ok_or(CordisError::UnknownFiber { fiber })?
        .machine
        .restart();
    Ok(schedule_transitions(state, transition))
}

fn reload_fiber(
    state: &mut SupervisorState,
    fiber: FiberId,
) -> Result<Vec<FiberTransition>, CordisError> {
    let transition = state
        .fibers
        .get_mut(&fiber)
        .ok_or(CordisError::UnknownFiber { fiber })?
        .machine
        .reload();
    Ok(schedule_transitions(state, transition))
}

fn validate_injects(fiber: FiberId, injects: &[InjectSpec]) -> Result<(), CordisError> {
    let mut services = BTreeSet::new();
    for inject in injects {
        if !services.insert(inject.service.clone()) {
            return Err(CordisError::DuplicateInject {
                fiber,
                service: inject.service.clone(),
            });
        }
    }
    Ok(())
}

fn resolve_dependencies(
    state: &SupervisorState,
    context: &Context,
    injects: &[InjectSpec],
) -> Result<DependencyResolution, CordisError> {
    injects
        .iter()
        .map(|inject| {
            let key = ProviderKey::new(
                inject.service.clone(),
                context.resolve_realm(&inject.service)?,
            );
            Ok(ResolvedInject {
                provider: state.providers.get(&key).copied(),
                key,
                requirement: inject.requirement,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(DependencyResolution::new)
}

fn recompute_affected(state: &mut SupervisorState, changed_key: &ProviderKey) -> Vec<FiberId> {
    let candidates = state
        .fibers
        .iter()
        .filter_map(|(fiber, record)| {
            let context = record.context.as_ref()?;
            record
                .injects
                .iter()
                .any(|inject| {
                    inject.service == changed_key.service
                        && context.resolve_realm(&inject.service).ok() == Some(changed_key.realm)
                })
                .then_some((*fiber, context.clone(), record.injects.clone()))
        })
        .collect::<Vec<_>>();

    let mut affected = Vec::with_capacity(candidates.len());
    for (fiber, context, injects) in candidates {
        let desired = resolve_dependencies(state, &context, &injects)
            .expect("configured context realms remain immutable");
        let record = state
            .fibers
            .get_mut(&fiber)
            .expect("candidate fiber came from the same state");
        if record.snapshot.desired != desired {
            record.snapshot.desired = desired;
            affected.push(fiber);
        }
    }
    affected
}

fn reconcile_lifecycles(state: &mut SupervisorState) -> Vec<FiberTransition> {
    let cycles = dependency_cycles(state);
    let fibers = state.fibers.keys().copied().collect::<Vec<_>>();
    let mut transitions = Vec::new();
    for fiber in fibers {
        let record = state
            .fibers
            .get_mut(&fiber)
            .expect("fiber id came from the same state");
        let desired = if record.retired {
            record.snapshot.dependency_error = None;
            DesiredState::Retired
        } else if record.context.is_none() {
            record.snapshot.dependency_error = None;
            DesiredState::Waiting
        } else if let Some(cycle) = cycles.get(&fiber) {
            record.snapshot.dependency_error = Some(CordisError::DependencyCycle {
                fibers: cycle.clone(),
            });
            DesiredState::Waiting
        } else {
            record.snapshot.dependency_error = None;
            desired_state(&record.snapshot.desired)
        };
        if let Some(transition) = record.machine.set_desired(desired) {
            transitions.push(transition);
        }
    }
    transitions
}

fn dependency_cycles(state: &SupervisorState) -> BTreeMap<FiberId, Vec<FiberId>> {
    fn visit(
        fiber: FiberId,
        graph: &BTreeMap<FiberId, Vec<FiberId>>,
        visited: &mut BTreeSet<FiberId>,
        finished: &mut Vec<FiberId>,
    ) {
        if !visited.insert(fiber) {
            return;
        }
        for provider in &graph[&fiber] {
            visit(*provider, graph, visited, finished);
        }
        finished.push(fiber);
    }

    fn collect(
        fiber: FiberId,
        graph: &BTreeMap<FiberId, Vec<FiberId>>,
        visited: &mut BTreeSet<FiberId>,
        component: &mut Vec<FiberId>,
    ) {
        if !visited.insert(fiber) {
            return;
        }
        component.push(fiber);
        for consumer in &graph[&fiber] {
            collect(*consumer, graph, visited, component);
        }
    }

    let graph = state
        .fibers
        .iter()
        .map(|(fiber, record)| {
            let providers = record.context.as_ref().map_or_else(Vec::new, |_| {
                record
                    .snapshot
                    .desired
                    .entries()
                    .iter()
                    .filter_map(|entry| entry.provider)
                    .collect::<Vec<_>>()
            });
            (*fiber, providers)
        })
        .collect::<BTreeMap<_, _>>();
    let mut reverse = graph
        .keys()
        .map(|fiber| (*fiber, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for (consumer, providers) in &graph {
        for provider in providers {
            reverse
                .get_mut(provider)
                .expect("all providers are runtime fibers")
                .push(*consumer);
        }
    }

    let mut visited = BTreeSet::new();
    let mut finished = Vec::with_capacity(graph.len());
    for fiber in graph.keys() {
        visit(*fiber, &graph, &mut visited, &mut finished);
    }

    visited.clear();
    let mut cycles = BTreeMap::new();
    for fiber in finished.into_iter().rev() {
        let mut component = Vec::new();
        collect(fiber, &reverse, &mut visited, &mut component);
        component.sort_unstable();
        let cyclic = component.len() > 1 || graph[&fiber].contains(&fiber);
        if cyclic {
            for member in &component {
                cycles.insert(*member, component.clone());
            }
        }
    }
    cycles
}

fn schedule_transitions(
    state: &mut SupervisorState,
    transition: Option<FiberTransition>,
) -> Vec<FiberTransition> {
    schedule_transition_batch(state, transition.into_iter().collect())
}

fn schedule_transition_batch(
    state: &mut SupervisorState,
    transitions: Vec<FiberTransition>,
) -> Vec<FiberTransition> {
    let mut queue = VecDeque::from(transitions);
    let mut ready = Vec::new();
    while let Some(transition) = queue.pop_front() {
        match transition.kind {
            crate::TransitionKind::Load { .. } => ready.push(transition),
            crate::TransitionKind::Unload => {
                let fiber = transition.fiber;
                state.blocked_unloads.insert(fiber, transition);
                queue.extend(withdraw_provider_bindings(state, fiber));
            }
        }
    }
    ready.extend(release_ready_unloads(state));
    ready
}

fn withdraw_provider_bindings(
    state: &mut SupervisorState,
    provider: FiberId,
) -> Vec<FiberTransition> {
    let keys = state
        .providers
        .iter()
        .filter_map(|(key, owner)| (*owner == provider).then_some(key.clone()))
        .collect::<Vec<_>>();
    for key in keys {
        state.providers.remove(&key);
        recompute_affected(state, &key);
    }
    reconcile_lifecycles(state)
}

fn release_ready_unloads(state: &mut SupervisorState) -> Vec<FiberTransition> {
    let fibers = state
        .blocked_unloads
        .keys()
        .copied()
        .filter(|provider| !has_active_consumers(state, *provider))
        .collect::<Vec<_>>();
    fibers
        .into_iter()
        .filter_map(|fiber| state.blocked_unloads.remove(&fiber))
        .collect()
}

fn has_active_consumers(state: &SupervisorState, provider: FiberId) -> bool {
    state.fibers.values().any(|record| {
        matches!(
            record.machine.state(),
            FiberState::Loading | FiberState::Active | FiberState::Unloading
        ) && record
            .snapshot
            .committed
            .as_ref()
            .is_some_and(|view| view.entries().any(|entry| entry.provider == Some(provider)))
    })
}

fn desired_state(resolution: &DependencyResolution) -> DesiredState {
    DesiredEpoch::from_resolution(resolution).map_or(DesiredState::Waiting, DesiredState::Ready)
}

fn snapshot(state: &SupervisorState) -> RuntimeSnapshot {
    RuntimeSnapshot {
        fibers: state
            .fibers
            .values()
            .map(|record| {
                let mut fiber = record.snapshot.clone();
                fiber.state = record.machine.state();
                fiber.active_transition = record.machine.active_transition().cloned();
                fiber.failure = record.machine.failure().cloned();
                fiber.teardown_error = record.machine.teardown_error().cloned();
                fiber
            })
            .collect(),
        allocated_realms: state.allocated_realms,
        provider_count: state.providers.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::join_all;
    use std::collections::BTreeSet;

    fn service(name: &str) -> crate::ServiceId {
        crate::ServiceId::new(name, [0; 32])
    }

    async fn activate(
        handle: &RuntimeHandle,
        fiber: FiberId,
        context: Context,
        injects: Vec<InjectSpec>,
    ) {
        let change = handle
            .configure_dependencies(fiber, context, injects)
            .await
            .unwrap();
        let load = change.transitions.first().unwrap();
        handle.commit_dependencies(fiber).await.unwrap();
        let update = handle
            .complete_transition(fiber, load.generation, Ok(()))
            .await
            .unwrap();
        assert_eq!(update.status, CompletionStatus::Applied);
        assert!(update.ready.is_empty());
    }

    #[tokio::test]
    async fn supervisor_serializes_concurrent_creates() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let creates = (0..32).map(|_| {
            let handle = handle.clone();
            tokio::spawn(async move { handle.create_fiber(None).await.unwrap() })
        });
        let ids = join_all(creates)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), 32);
        assert_eq!(handle.snapshot().await.unwrap().fibers.len(), 32);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn create_fiber_validates_parent() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let root = handle.create_fiber(None).await.unwrap();
        assert_eq!(
            handle.create_live_child_fiber(root).await,
            Err(CordisError::InactiveFiber { fiber: root })
        );
        let child = handle.create_fiber(Some(root)).await.unwrap();
        let unknown = FiberId::next();

        assert_eq!(
            handle.create_fiber(Some(unknown)).await,
            Err(CordisError::UnknownFiber { fiber: unknown })
        );
        let snapshot = runtime.shutdown().await.unwrap();
        assert_eq!(snapshot.fibers.len(), 2);
        assert_eq!(snapshot.fibers[1].parent, Some(root));
        assert_eq!(snapshot.fibers[1].id, child);
    }

    #[tokio::test]
    async fn shutdown_closes_existing_handles() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let realm = handle.allocate_realm().await.unwrap();
        let snapshot = runtime.shutdown().await.unwrap();

        assert!(realm.get() > 0);
        assert_eq!(snapshot.allocated_realms, 1);
        assert_eq!(handle.snapshot().await, Err(CordisError::RuntimeClosed));
    }

    #[tokio::test]
    async fn provider_changes_notify_only_matching_realm_consumers() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let provider = handle.create_fiber(None).await.unwrap();
        let consumer_a = handle.create_fiber(None).await.unwrap();
        let consumer_b = handle.create_fiber(None).await.unwrap();
        let database = service("database");
        let realm_a = handle.allocate_realm().await.unwrap();
        let realm_b = handle.allocate_realm().await.unwrap();
        let context_a = Context::root(consumer_a).isolate(database.clone(), realm_a);
        let context_b = Context::root(consumer_b).isolate(database.clone(), realm_b);

        let unresolved = handle
            .configure_dependencies(
                consumer_a,
                context_a,
                vec![InjectSpec::required(database.clone())],
            )
            .await
            .unwrap();
        handle
            .configure_dependencies(
                consumer_b,
                context_b,
                vec![InjectSpec::required(database.clone())],
            )
            .await
            .unwrap();
        assert!(!unresolved.resolution.is_ready());

        let key_a = ProviderKey::new(database.clone(), realm_a);
        let change = handle.provide(key_a.clone(), provider).await.unwrap();
        assert_eq!(change.affected, vec![consumer_a]);
        let load = change.transitions.first().unwrap().clone();
        let committed = handle.commit_dependencies(consumer_a).await.unwrap();
        assert_eq!(committed.lookup(&database), Ok(Some(provider)));

        let change = handle.withdraw(key_a.clone(), provider).await.unwrap();
        assert_eq!(change.affected, vec![consumer_a]);
        assert!(change.transitions.is_empty());
        assert_eq!(
            handle.commit_dependencies(consumer_a).await,
            Err(CordisError::InactiveDependency { key: key_a })
        );
        let snapshot = handle.snapshot().await.unwrap();
        let consumer = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == consumer_a)
            .unwrap();
        assert_eq!(
            consumer.committed.as_ref().unwrap().lookup(&database),
            Ok(Some(provider))
        );

        let update = handle
            .complete_transition(consumer_a, load.generation, Ok(()))
            .await
            .unwrap();
        assert_eq!(update.status, CompletionStatus::Applied);
        let unload = update.ready.first().unwrap().clone();
        handle
            .complete_transition(consumer_a, unload.generation, Ok(()))
            .await
            .unwrap();
        let snapshot = runtime.shutdown().await.unwrap();
        let consumer = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == consumer_a)
            .unwrap();
        assert_eq!(consumer.state, FiberState::Pending);
        assert!(consumer.committed.is_none());
    }

    #[tokio::test]
    async fn provider_slots_are_unique_and_owner_checked() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let first = handle.create_fiber(None).await.unwrap();
        let second = handle.create_fiber(None).await.unwrap();
        let key = ProviderKey::new(service("database"), handle.allocate_realm().await.unwrap());

        handle.provide(key.clone(), first).await.unwrap();
        assert_eq!(
            handle.provide(key.clone(), second).await,
            Err(CordisError::DuplicateProvider {
                key: key.clone(),
                provider: first,
            })
        );
        assert_eq!(
            handle.withdraw(key.clone(), second).await,
            Err(CordisError::ProviderOwnershipMismatch {
                key: key.clone(),
                expected: second,
                actual: first,
            })
        );
        handle.withdraw(key, first).await.unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_inject_is_rejected_before_state_changes() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let fiber = handle.create_fiber(None).await.unwrap();
        let database = service("database");
        let realm = handle.allocate_realm().await.unwrap();
        let context = Context::root(fiber).isolate(database.clone(), realm);

        assert_eq!(
            handle
                .configure_dependencies(
                    fiber,
                    context,
                    vec![
                        InjectSpec::required(database.clone()),
                        InjectSpec::optional(database.clone()),
                    ],
                )
                .await,
            Err(CordisError::DuplicateInject {
                fiber,
                service: database,
            })
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dependency_cycle_stays_pending_and_recovers_when_broken() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let fiber = handle.create_fiber(None).await.unwrap();
        let recursive = service("recursive");
        let realm = handle.allocate_realm().await.unwrap();
        let key = ProviderKey::new(recursive.clone(), realm);
        handle.provide(key.clone(), fiber).await.unwrap();

        let change = handle
            .configure_dependencies(
                fiber,
                Context::root(fiber).isolate(recursive.clone(), realm),
                vec![InjectSpec::required(recursive)],
            )
            .await
            .unwrap();
        assert!(change.transitions.is_empty());
        let snapshot = handle.snapshot().await.unwrap();
        assert_eq!(snapshot.fibers[0].state, FiberState::Pending);
        assert_eq!(
            snapshot.fibers[0].dependency_error,
            Some(CordisError::DependencyCycle {
                fibers: vec![fiber],
            })
        );

        let change = handle.withdraw(key, fiber).await.unwrap();
        assert!(change.transitions.is_empty());
        let snapshot = runtime.shutdown().await.unwrap();
        assert_eq!(snapshot.fibers[0].state, FiberState::Pending);
        assert!(snapshot.fibers[0].dependency_error.is_none());
    }

    #[tokio::test]
    async fn dependency_cycle_reports_every_scc_member() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let first = handle.create_fiber(None).await.unwrap();
        let second = handle.create_fiber(None).await.unwrap();
        let first_service = service("first");
        let second_service = service("second");
        let first_realm = handle.allocate_realm().await.unwrap();
        let second_realm = handle.allocate_realm().await.unwrap();
        handle
            .provide(ProviderKey::new(first_service.clone(), first_realm), first)
            .await
            .unwrap();
        handle
            .provide(
                ProviderKey::new(second_service.clone(), second_realm),
                second,
            )
            .await
            .unwrap();

        handle
            .configure_dependencies(
                first,
                Context::root(first).isolate(second_service.clone(), second_realm),
                vec![InjectSpec::required(second_service)],
            )
            .await
            .unwrap();
        let change = handle
            .configure_dependencies(
                second,
                Context::root(second).isolate(first_service.clone(), first_realm),
                vec![InjectSpec::required(first_service)],
            )
            .await
            .unwrap();
        assert!(change.transitions.is_empty());

        let expected = Some(CordisError::DependencyCycle {
            fibers: vec![first, second],
        });
        let snapshot = runtime.shutdown().await.unwrap();
        assert_eq!(snapshot.fibers[0].dependency_error, expected.clone());
        assert_eq!(snapshot.fibers[1].dependency_error, expected);
        assert_eq!(snapshot.fibers[1].state, FiberState::Pending);
    }

    #[tokio::test]
    async fn teardown_drains_consumers_before_providers() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let provider = handle.create_fiber(None).await.unwrap();
        let middle = handle.create_fiber(None).await.unwrap();
        let leaf = handle.create_fiber(None).await.unwrap();
        let database = service("database");
        let repository = service("repository");
        let database_realm = handle.allocate_realm().await.unwrap();
        let repository_realm = handle.allocate_realm().await.unwrap();

        activate(&handle, provider, Context::root(provider), Vec::new()).await;
        handle
            .provide(ProviderKey::new(database.clone(), database_realm), provider)
            .await
            .unwrap();
        activate(
            &handle,
            middle,
            Context::root(middle).isolate(database.clone(), database_realm),
            vec![InjectSpec::required(database)],
        )
        .await;
        handle
            .provide(
                ProviderKey::new(repository.clone(), repository_realm),
                middle,
            )
            .await
            .unwrap();
        activate(
            &handle,
            leaf,
            Context::root(leaf).isolate(repository.clone(), repository_realm),
            vec![InjectSpec::required(repository)],
        )
        .await;

        let ready = handle.retire_fiber(provider).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].fiber, leaf);
        let snapshot = handle.snapshot().await.unwrap();
        let blocked_middle = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == middle)
            .and_then(|fiber| fiber.active_transition.clone())
            .unwrap();
        assert_eq!(
            handle
                .complete_transition(middle, blocked_middle.generation, Ok(()))
                .await,
            Err(CordisError::TransitionBlocked { fiber: middle })
        );

        let update = handle
            .complete_transition(leaf, ready[0].generation, Ok(()))
            .await
            .unwrap();
        assert_eq!(update.ready.len(), 1);
        assert_eq!(update.ready[0].fiber, middle);
        let update = handle
            .complete_transition(middle, update.ready[0].generation, Ok(()))
            .await
            .unwrap();
        assert_eq!(update.ready.len(), 1);
        assert_eq!(update.ready[0].fiber, provider);
        let update = handle
            .complete_transition(provider, update.ready[0].generation, Ok(()))
            .await
            .unwrap();
        assert!(update.ready.is_empty());

        let snapshot = runtime.shutdown().await.unwrap();
        let states = snapshot
            .fibers
            .iter()
            .map(|fiber| (fiber.id, fiber.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states[&leaf], FiberState::Pending);
        assert_eq!(states[&middle], FiberState::Pending);
        assert_eq!(states[&provider], FiberState::Disposed);
        assert_eq!(snapshot.provider_count, 0);
    }

    #[tokio::test]
    async fn failed_load_withdraws_partial_providers_and_requires_restart() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let dependency = handle.create_fiber(None).await.unwrap();
        let component = handle.create_fiber(None).await.unwrap();
        let consumer = handle.create_fiber(None).await.unwrap();
        let input = service("input");
        let output = service("output");
        let input_realm = handle.allocate_realm().await.unwrap();
        let output_realm = handle.allocate_realm().await.unwrap();
        let input_key = ProviderKey::new(input.clone(), input_realm);
        let output_key = ProviderKey::new(output.clone(), output_realm);

        handle.provide(input_key.clone(), dependency).await.unwrap();
        let change = handle
            .configure_dependencies(
                component,
                Context::root(component).isolate(input.clone(), input_realm),
                vec![InjectSpec::required(input)],
            )
            .await
            .unwrap();
        let component_load = change.transitions[0].clone();
        handle.commit_dependencies(component).await.unwrap();

        handle.provide(output_key, component).await.unwrap();
        activate(
            &handle,
            consumer,
            Context::root(consumer).isolate(output.clone(), output_realm),
            vec![InjectSpec::required(output)],
        )
        .await;

        let failure = CordisError::DisposerFailed {
            message: "apply failed".to_owned(),
        };
        let update = handle
            .complete_transition(component, component_load.generation, Err(failure.clone()))
            .await
            .unwrap();
        assert_eq!(update.ready.len(), 1);
        assert_eq!(update.ready[0].fiber, consumer);

        let snapshot = handle.snapshot().await.unwrap();
        let failed = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == component)
            .unwrap();
        assert_eq!(failed.state, FiberState::Failed);
        assert_eq!(failed.failure.as_ref(), Some(&failure));
        assert!(failed.committed.is_none());
        assert_eq!(snapshot.provider_count, 1);

        handle
            .complete_transition(consumer, update.ready[0].generation, Ok(()))
            .await
            .unwrap();
        let change = handle
            .withdraw(input_key.clone(), dependency)
            .await
            .unwrap();
        assert!(change.transitions.is_empty());
        let change = handle.provide(input_key, dependency).await.unwrap();
        assert!(change.transitions.is_empty());

        let ready = handle.restart_fiber(component).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].fiber, component);
        handle.commit_dependencies(component).await.unwrap();
        handle
            .complete_transition(component, ready[0].generation, Ok(()))
            .await
            .unwrap();

        let snapshot = runtime.shutdown().await.unwrap();
        let recovered = snapshot
            .fibers
            .iter()
            .find(|fiber| fiber.id == component)
            .unwrap();
        assert_eq!(recovered.state, FiberState::Active);
        assert!(recovered.failure.is_none());
    }

    #[tokio::test]
    async fn explicit_dependency_update_recovers_failed_fiber() {
        let runtime = Runtime::start();
        let handle = runtime.handle();
        let fiber = handle.create_fiber(None).await.unwrap();
        let context = Context::root(fiber);
        let change = handle
            .configure_dependencies(fiber, context.clone(), Vec::new())
            .await
            .unwrap();
        handle.commit_dependencies(fiber).await.unwrap();
        handle
            .complete_transition(
                fiber,
                change.transitions[0].generation,
                Err(CordisError::DisposerFailed {
                    message: "invalid config".to_owned(),
                }),
            )
            .await
            .unwrap();

        let update = handle
            .configure_dependencies(fiber, context, Vec::new())
            .await
            .unwrap();
        assert_eq!(update.transitions.len(), 1);
        handle.commit_dependencies(fiber).await.unwrap();
        handle
            .complete_transition(fiber, update.transitions[0].generation, Ok(()))
            .await
            .unwrap();

        let snapshot = runtime.shutdown().await.unwrap();
        assert_eq!(snapshot.fibers[0].state, FiberState::Active);
        assert!(snapshot.fibers[0].failure.is_none());
    }
}
