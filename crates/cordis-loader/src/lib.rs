//! Declarative entry tree, configuration includes, and transactional reconciliation.

mod entry;
mod include;

pub use entry::{
    ComponentRef, EntryDriver, EntryId, EntrySpec, EntryTree, IsolationRule, LoaderError,
    LoaderFuture, ManagedRealm, ResolvedEntry,
};
pub use include::{ExprEvaluator, IncludeDocument, IncludeFormat, Patch, PatchAction};
