use crate::{EffectId, FiberId, ProviderKey, ServiceId};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CordisError {
    #[error("cannot add a disposer to inactive effect {effect}")]
    InactiveEffect { effect: EffectId },

    #[error("no realm is visible for service {service}")]
    MissingRealm { service: ServiceId },

    #[error("fiber {fiber} does not exist")]
    UnknownFiber { fiber: FiberId },

    #[error("context belongs to fiber {actual}, expected fiber {expected}")]
    ContextFiberMismatch { expected: FiberId, actual: FiberId },

    #[error("fiber {fiber} declares service {service} more than once")]
    DuplicateInject { fiber: FiberId, service: ServiceId },

    #[error("provider slot {key} is already owned by fiber {provider}")]
    DuplicateProvider { key: ProviderKey, provider: FiberId },

    #[error("provider slot {key} does not exist")]
    ProviderNotFound { key: ProviderKey },

    #[error("provider slot {key} belongs to fiber {actual}, not {expected}")]
    ProviderOwnershipMismatch {
        key: ProviderKey,
        expected: FiberId,
        actual: FiberId,
    },

    #[error("required dependency {key} is inactive")]
    InactiveDependency { key: ProviderKey },

    #[error("service {service} was not declared by this fiber")]
    UndeclaredDependency { service: ServiceId },

    #[error("runtime supervisor is closed")]
    RuntimeClosed,

    #[error("runtime supervisor task failed: {message}")]
    SupervisorFailed { message: String },

    #[error("fiber {fiber} unload is waiting for active consumers")]
    TransitionBlocked { fiber: FiberId },

    #[error("child effect {effect} failed during disposal")]
    ChildEffectFailed { effect: EffectId, errors: Vec<Self> },

    #[error("effect disposer panicked: {message}")]
    DisposerPanic { message: String },

    #[error("effect disposer failed: {message}")]
    DisposerFailed { message: String },
}
