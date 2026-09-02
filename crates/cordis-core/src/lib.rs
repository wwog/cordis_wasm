//! Core runtime semantics for Cordis.
//!
//! This crate intentionally contains no Wasmtime dependency. Native and WebAssembly
//! components must use the same effect, service, event, and fiber machinery.

mod context;
mod effect;
mod error;
mod event;
mod fiber;
mod id;
mod service;
mod supervisor;

pub use context::Context;
pub use effect::{
    DisposeErrors, DisposeReport, Disposer, EffectGuard, EffectMeta, EffectScope, EffectSet,
};
pub use error::CordisError;
pub use event::{AsyncEvent, BailEvent, EventTarget, ListenerOptions, Next, WaterfallEvent};
pub use fiber::{
    DesiredEpoch, DesiredState, EpochEntry, FiberMachine, FiberState, FiberTransition,
    TransitionAdvance, TransitionKind,
};
pub use id::{EffectId, FiberId, ListenerId, RealmId};
pub use service::{
    CommittedView, DependencyResolution, InjectSpec, ProviderKey, RegistryChange, Requirement,
    ResolvedInject, ServiceId, ServiceKey,
};
pub use supervisor::{
    CompletionStatus, DependencyChange, FiberSnapshot, Runtime, RuntimeHandle, RuntimeSnapshot,
    TransitionUpdate,
};
