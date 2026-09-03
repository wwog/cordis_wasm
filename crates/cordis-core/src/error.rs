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

    #[error("fiber {fiber} is not loading or active")]
    InactiveFiber { fiber: FiberId },

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

    #[error("service dispatcher identity mismatch: expected {expected}, got {actual}")]
    ServiceIdentityMismatch {
        expected: ServiceId,
        actual: ServiceId,
    },

    #[error("component context has no method-fiber runtime")]
    MissingMethodRuntime,

    #[error("provider fiber {provider} has no dispatcher for service {service}")]
    MissingServiceDispatcher {
        provider: FiberId,
        service: ServiceId,
    },

    #[error("committed dependency {service} has no provider")]
    MissingCommittedProvider { service: ServiceId },

    #[error("service {service} has no method with id {method_id:#010x}")]
    UnknownServiceMethod { service: ServiceId, method_id: u32 },

    #[error("failed to encode service payload: {message}")]
    ServiceEncodeFailed { message: String },

    #[error("failed to decode service payload: {message}")]
    ServiceDecodeFailed { message: String },

    #[error("failed to encode event payload: {message}")]
    EventEncodeFailed { message: String },

    #[error("failed to decode event payload: {message}")]
    EventDecodeFailed { message: String },

    #[error("dynamic payload is {actual} bytes, limit is {limit} bytes")]
    PayloadLimitExceeded { actual: usize, limit: usize },

    #[error("kernel ABI mismatch: expected {expected}, got {actual}")]
    KernelAbiMismatch { expected: String, actual: String },

    #[error("component capability `{capability}` is denied")]
    CapabilityDenied { capability: String },

    #[error("component `{component}` configuration is invalid at {path}: {message}")]
    InvalidComponentConfig {
        component: String,
        path: String,
        message: String,
    },

    #[error("reentrant call into component fiber {fiber} is not allowed")]
    ReentrantCall { fiber: FiberId },

    #[error("component `{component}` failed: {message}")]
    ComponentFailed { component: String, message: String },

    #[error("runtime supervisor is closed")]
    RuntimeClosed,

    #[error("runtime supervisor task failed: {message}")]
    SupervisorFailed { message: String },

    #[error("fiber {fiber} executor panicked: {message}")]
    FiberExecutorPanicked { fiber: FiberId, message: String },

    #[error("fiber {fiber} unload is waiting for active consumers")]
    TransitionBlocked { fiber: FiberId },

    #[error("dependency cycle contains fibers {fibers:?}")]
    DependencyCycle { fibers: Vec<FiberId> },

    #[error("event listeners failed: {errors:?}")]
    EventListenersFailed { errors: Vec<Self> },

    #[error("event listener panicked: {message}")]
    EventListenerPanicked { message: String },

    #[error("waterfall next was already called")]
    NextAlreadyUsed,

    #[error("child effect {effect} failed during disposal")]
    ChildEffectFailed { effect: EffectId, errors: Vec<Self> },

    #[error("effect disposer panicked: {message}")]
    DisposerPanic { message: String },

    #[error("effect disposer failed: {message}")]
    DisposerFailed { message: String },
}
