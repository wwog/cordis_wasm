use crate::{CordisError, DependencyResolution, FiberId, ProviderKey};
use std::sync::Arc;

/// Stable lifecycle state visible to diagnostics and waiters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Failed,
    Unloading,
    Disposed,
}

/// One provider selection included in a desired epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochEntry {
    pub key: ProviderKey,
    pub provider: Option<FiberId>,
}

/// Ordered provider selection for one load attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredEpoch {
    entries: Arc<[EpochEntry]>,
}

impl DesiredEpoch {
    pub fn from_resolution(resolution: &DependencyResolution) -> Option<Self> {
        resolution.is_ready().then(|| Self {
            entries: resolution
                .entries()
                .iter()
                .map(|entry| EpochEntry {
                    key: entry.key.clone(),
                    provider: entry.provider,
                })
                .collect(),
        })
    }

    pub fn entries(&self) -> &[EpochEntry] {
        &self.entries
    }
}

/// Latest lifecycle target. Changes coalesce while a transition is running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesiredState {
    Waiting,
    Ready(DesiredEpoch),
    Retired,
}

/// Work that must run outside the supervisor task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberTransition {
    pub fiber: FiberId,
    pub generation: u64,
    pub kind: TransitionKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Load { epoch: DesiredEpoch },
    Unload,
}

/// Result of applying a transition completion to the current machine generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionAdvance {
    IgnoredStale,
    Settled,
    Start(FiberTransition),
}

/// Pure state machine behind one supervisor-owned fiber record.
#[derive(Clone, Debug)]
pub struct FiberMachine {
    fiber: FiberId,
    state: FiberState,
    desired: DesiredState,
    active: Option<FiberTransition>,
    loaded_epoch: Option<DesiredEpoch>,
    next_generation: u64,
    failure: Option<Arc<CordisError>>,
    teardown_error: Option<Arc<CordisError>>,
}

impl FiberMachine {
    pub fn new(fiber: FiberId) -> Self {
        Self {
            fiber,
            state: FiberState::Pending,
            desired: DesiredState::Waiting,
            active: None,
            loaded_epoch: None,
            next_generation: 1,
            failure: None,
            teardown_error: None,
        }
    }

    pub const fn state(&self) -> FiberState {
        self.state
    }

    pub fn desired(&self) -> &DesiredState {
        &self.desired
    }

    pub fn active_transition(&self) -> Option<&FiberTransition> {
        self.active.as_ref()
    }

    pub fn failure(&self) -> Option<&CordisError> {
        self.failure.as_deref()
    }

    pub fn teardown_error(&self) -> Option<&CordisError> {
        self.teardown_error.as_deref()
    }

    /// Updates the target and starts work only when the machine is idle.
    pub fn set_desired(&mut self, desired: DesiredState) -> Option<FiberTransition> {
        self.desired = desired;
        if self.active.is_some() {
            return None;
        }

        match (&self.state, &self.desired) {
            (FiberState::Pending, DesiredState::Ready(epoch)) => {
                Some(self.start(TransitionKind::Load {
                    epoch: epoch.clone(),
                }))
            }
            (FiberState::Pending | FiberState::Failed, DesiredState::Retired) => {
                self.state = FiberState::Disposed;
                None
            }
            (FiberState::Active, DesiredState::Ready(epoch))
                if self.loaded_epoch.as_ref() == Some(epoch) =>
            {
                None
            }
            (FiberState::Active, _) => Some(self.start(TransitionKind::Unload)),
            _ => None,
        }
    }

    /// Completes one externally executed transition.
    pub fn complete(
        &mut self,
        generation: u64,
        result: Result<(), CordisError>,
    ) -> TransitionAdvance {
        let Some(active) = self.active.as_ref() else {
            return TransitionAdvance::IgnoredStale;
        };
        if active.generation != generation {
            return TransitionAdvance::IgnoredStale;
        }
        let kind = active.kind.clone();
        self.active = None;

        match kind {
            TransitionKind::Load { epoch } => self.complete_load(epoch, result),
            TransitionKind::Unload => self.complete_unload(result),
        }
    }

    /// Explicitly retries a failed fiber against its latest desired state.
    pub fn restart(&mut self) -> Option<FiberTransition> {
        if self.state != FiberState::Failed {
            return None;
        }
        self.failure = None;
        match &self.desired {
            DesiredState::Waiting => {
                self.state = FiberState::Pending;
                None
            }
            DesiredState::Ready(epoch) => Some(self.start(TransitionKind::Load {
                epoch: epoch.clone(),
            })),
            DesiredState::Retired => {
                self.state = FiberState::Disposed;
                None
            }
        }
    }

    /// Forces an active fiber through unload and a fresh load of its desired epoch.
    pub fn reload(&mut self) -> Option<FiberTransition> {
        if self.active.is_none()
            && self.state == FiberState::Active
            && matches!(self.desired, DesiredState::Ready(_))
        {
            Some(self.start(TransitionKind::Unload))
        } else {
            None
        }
    }

    fn complete_load(
        &mut self,
        epoch: DesiredEpoch,
        result: Result<(), CordisError>,
    ) -> TransitionAdvance {
        if let Err(error) = result {
            self.failure = Some(Arc::new(error));
            self.state = FiberState::Failed;
            return TransitionAdvance::Settled;
        }

        let is_current = matches!(&self.desired, DesiredState::Ready(desired) if desired == &epoch);
        self.loaded_epoch = Some(epoch);

        if is_current {
            self.state = FiberState::Active;
            TransitionAdvance::Settled
        } else {
            TransitionAdvance::Start(self.start(TransitionKind::Unload))
        }
    }

    fn complete_unload(&mut self, result: Result<(), CordisError>) -> TransitionAdvance {
        self.teardown_error = result.err().map(Arc::new);
        self.loaded_epoch = None;
        match &self.desired {
            DesiredState::Waiting => {
                self.state = FiberState::Pending;
                TransitionAdvance::Settled
            }
            DesiredState::Ready(epoch) => {
                TransitionAdvance::Start(self.start(TransitionKind::Load {
                    epoch: epoch.clone(),
                }))
            }
            DesiredState::Retired => {
                self.state = FiberState::Disposed;
                TransitionAdvance::Settled
            }
        }
    }

    fn start(&mut self, kind: TransitionKind) -> FiberTransition {
        self.state = match kind {
            TransitionKind::Load { .. } => FiberState::Loading,
            TransitionKind::Unload => FiberState::Unloading,
        };
        let transition = FiberTransition {
            fiber: self.fiber,
            generation: self.next_generation,
            kind,
        };
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("fiber transition generation exhausted");
        self.active = Some(transition.clone());
        transition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RealmId, Requirement, ResolvedInject, ServiceId};

    fn epoch(provider: FiberId) -> DesiredEpoch {
        let resolution = DependencyResolution::new(vec![ResolvedInject {
            key: ProviderKey::new(ServiceId::new("database", [0; 32]), RealmId::next()),
            provider: Some(provider),
            requirement: Requirement::Required,
        }]);
        DesiredEpoch::from_resolution(&resolution).unwrap()
    }

    #[test]
    fn load_reaches_active_only_for_the_same_epoch() {
        let fiber = FiberId::next();
        let provider = FiberId::next();
        let desired = epoch(provider);
        let mut machine = FiberMachine::new(fiber);
        let load = machine
            .set_desired(DesiredState::Ready(desired.clone()))
            .unwrap();

        assert_eq!(machine.state(), FiberState::Loading);
        assert_eq!(
            machine.complete(load.generation, Ok(())),
            TransitionAdvance::Settled
        );
        assert_eq!(machine.state(), FiberState::Active);
        assert_eq!(machine.desired(), &DesiredState::Ready(desired));
    }

    #[test]
    fn desired_changes_coalesce_during_load() {
        let fiber = FiberId::next();
        let first_epoch = epoch(FiberId::next());
        let second_epoch = epoch(FiberId::next());
        let mut machine = FiberMachine::new(fiber);
        let load = machine
            .set_desired(DesiredState::Ready(first_epoch))
            .unwrap();
        assert_eq!(
            machine.set_desired(DesiredState::Ready(second_epoch.clone())),
            None
        );

        let unload = match machine.complete(load.generation, Ok(())) {
            TransitionAdvance::Start(transition) => transition,
            other => panic!("expected unload, got {other:?}"),
        };
        assert_eq!(unload.kind, TransitionKind::Unload);
        let reload = match machine.complete(unload.generation, Ok(())) {
            TransitionAdvance::Start(transition) => transition,
            other => panic!("expected reload, got {other:?}"),
        };
        assert_eq!(
            reload.kind,
            TransitionKind::Load {
                epoch: second_epoch
            }
        );
    }

    #[test]
    fn missing_dependencies_unload_an_active_fiber() {
        let mut machine = FiberMachine::new(FiberId::next());
        let load = machine
            .set_desired(DesiredState::Ready(epoch(FiberId::next())))
            .unwrap();
        machine.complete(load.generation, Ok(()));

        let unload = machine.set_desired(DesiredState::Waiting).unwrap();
        assert_eq!(machine.state(), FiberState::Unloading);
        assert_eq!(
            machine.complete(unload.generation, Ok(())),
            TransitionAdvance::Settled
        );
        assert_eq!(machine.state(), FiberState::Pending);
    }

    #[test]
    fn failed_load_requires_explicit_restart() {
        let mut machine = FiberMachine::new(FiberId::next());
        let desired = epoch(FiberId::next());
        let load = machine
            .set_desired(DesiredState::Ready(desired.clone()))
            .unwrap();
        let failure = CordisError::DisposerFailed {
            message: "apply failed".to_owned(),
        };
        machine.complete(load.generation, Err(failure.clone()));

        assert_eq!(machine.state(), FiberState::Failed);
        assert_eq!(machine.failure(), Some(&failure));
        assert_eq!(machine.set_desired(DesiredState::Ready(desired)), None);
        assert!(machine.restart().is_some());
        assert_eq!(machine.state(), FiberState::Loading);
    }

    #[test]
    fn stale_completion_cannot_change_current_transition() {
        let mut machine = FiberMachine::new(FiberId::next());
        let load = machine
            .set_desired(DesiredState::Ready(epoch(FiberId::next())))
            .unwrap();
        assert_eq!(
            machine.complete(load.generation + 1, Ok(())),
            TransitionAdvance::IgnoredStale
        );
        assert_eq!(machine.active_transition(), Some(&load));
    }

    #[test]
    fn unload_failure_is_recorded_but_does_not_block_retirement() {
        let mut machine = FiberMachine::new(FiberId::next());
        let load = machine
            .set_desired(DesiredState::Ready(epoch(FiberId::next())))
            .unwrap();
        machine.complete(load.generation, Ok(()));
        let unload = machine.set_desired(DesiredState::Retired).unwrap();
        let failure = CordisError::DisposerFailed {
            message: "cleanup failed".to_owned(),
        };

        machine.complete(unload.generation, Err(failure.clone()));
        assert_eq!(machine.state(), FiberState::Disposed);
        assert_eq!(machine.teardown_error(), Some(&failure));
    }

    #[test]
    fn optional_missing_provider_still_forms_an_epoch() {
        let resolution = DependencyResolution::new(vec![ResolvedInject {
            key: ProviderKey::new(ServiceId::new("optional", [1; 32]), RealmId::next()),
            provider: None,
            requirement: Requirement::Optional,
        }]);
        assert!(DesiredEpoch::from_resolution(&resolution).is_some());
    }

    #[test]
    fn generated_transition_sequences_preserve_state_invariants() {
        for seed in 1_u64..=128 {
            let fiber = FiberId::next();
            let mut machine = FiberMachine::new(fiber);
            let ready = epoch(FiberId::next());
            let mut random = seed;
            let mut disposed = false;
            for _ in 0..256 {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                match random % 7 {
                    0 => {
                        machine.set_desired(DesiredState::Waiting);
                    }
                    1 => {
                        machine.set_desired(DesiredState::Ready(ready.clone()));
                    }
                    2 => {
                        machine.set_desired(DesiredState::Retired);
                    }
                    3 => {
                        machine.restart();
                    }
                    4 => {
                        machine.reload();
                    }
                    5 | 6 => {
                        if let Some(active) = machine.active_transition().cloned() {
                            let generation = if random % 2 == 0 {
                                active.generation
                            } else {
                                active.generation.saturating_add(1)
                            };
                            let result = if random & 8 == 0 {
                                Ok(())
                            } else {
                                Err(CordisError::ComponentFailed {
                                    component: "generated".into(),
                                    message: "injected".into(),
                                })
                            };
                            machine.complete(generation, result);
                        }
                    }
                    _ => unreachable!(),
                }

                let transitioning =
                    matches!(machine.state(), FiberState::Loading | FiberState::Unloading);
                assert_eq!(machine.active_transition().is_some(), transitioning);
                if disposed {
                    assert_eq!(machine.state(), FiberState::Disposed);
                }
                disposed |= machine.state() == FiberState::Disposed;
            }
        }
    }
}
