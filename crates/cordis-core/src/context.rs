use crate::{CordisError, FiberId, RealmId, ServiceId};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A cheap, immutable view of service realms and intercept layers.
#[derive(Clone, Debug)]
pub struct Context {
    node: Arc<ContextNode>,
}

#[derive(Debug)]
struct ContextNode {
    parent: Option<Arc<ContextNode>>,
    fiber: FiberId,
    realms: BTreeMap<ServiceId, RealmId>,
    intercepts: BTreeMap<ServiceId, Arc<Value>>,
}

impl Context {
    pub fn root(fiber: FiberId) -> Self {
        Self {
            node: Arc::new(ContextNode {
                parent: None,
                fiber,
                realms: BTreeMap::new(),
                intercepts: BTreeMap::new(),
            }),
        }
    }

    pub fn fiber(&self) -> FiberId {
        self.node.fiber
    }

    /// Creates an empty overlay belonging to another fiber.
    #[must_use]
    pub fn extend(&self, fiber: FiberId) -> Self {
        self.overlay(fiber, BTreeMap::new(), BTreeMap::new())
    }

    /// Overrides one service realm in a new overlay.
    #[must_use]
    pub fn isolate(&self, service: ServiceId, realm: RealmId) -> Self {
        self.overlay(
            self.fiber(),
            BTreeMap::from([(service, realm)]),
            BTreeMap::new(),
        )
    }

    /// Adds one dynamic intercept layer in a new overlay.
    #[must_use]
    pub fn intercept(&self, service: ServiceId, value: Value) -> Self {
        self.overlay(
            self.fiber(),
            BTreeMap::new(),
            BTreeMap::from([(service, Arc::new(value))]),
        )
    }

    /// Resolves the nearest realm override for a service.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::MissingRealm`] when no overlay defines the service.
    pub fn resolve_realm(&self, service: &ServiceId) -> Result<RealmId, CordisError> {
        self.ancestors()
            .find_map(|node| node.realms.get(service).copied())
            .ok_or_else(|| CordisError::MissingRealm {
                service: service.clone(),
            })
    }

    /// Returns intercept values from the outermost layer to the innermost layer.
    pub fn intercept_layers(&self, service: &ServiceId) -> Vec<Arc<Value>> {
        let mut layers = self
            .ancestors()
            .filter_map(|node| node.intercepts.get(service).cloned())
            .collect::<Vec<_>>();
        layers.reverse();
        layers
    }

    fn overlay(
        &self,
        fiber: FiberId,
        realms: BTreeMap<ServiceId, RealmId>,
        intercepts: BTreeMap<ServiceId, Arc<Value>>,
    ) -> Self {
        Self {
            node: Arc::new(ContextNode {
                parent: Some(self.node.clone()),
                fiber,
                realms,
                intercepts,
            }),
        }
    }

    fn ancestors(&self) -> impl Iterator<Item = &ContextNode> {
        std::iter::successors(Some(self.node.as_ref()), |node| node.parent.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(name: &str) -> ServiceId {
        ServiceId::new(name, [0; 32])
    }

    #[test]
    fn overlays_resolve_nearest_realm_without_mutating_parent() {
        let database = service("database");
        let root_realm = RealmId::next();
        let local_realm = RealmId::next();
        let root = Context::root(FiberId::next()).isolate(database.clone(), root_realm);
        let local = root.isolate(database.clone(), local_realm);

        assert_eq!(root.resolve_realm(&database), Ok(root_realm));
        assert_eq!(local.resolve_realm(&database), Ok(local_realm));
    }

    #[test]
    fn extend_changes_owner_and_inherits_overlays() {
        let database = service("database");
        let realm = RealmId::next();
        let parent_fiber = FiberId::next();
        let child_fiber = FiberId::next();
        let parent = Context::root(parent_fiber).isolate(database.clone(), realm);
        let child = parent.extend(child_fiber);

        assert_eq!(parent.fiber(), parent_fiber);
        assert_eq!(child.fiber(), child_fiber);
        assert_eq!(child.resolve_realm(&database), Ok(realm));
    }

    #[test]
    fn intercept_layers_are_returned_outermost_first() {
        let logger = service("logger");
        let root = Context::root(FiberId::next())
            .intercept(logger.clone(), serde_json::json!({ "level": "info" }));
        let child = root
            .extend(FiberId::next())
            .intercept(logger.clone(), serde_json::json!({ "target": "db" }));

        let layers = child.intercept_layers(&logger);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0]["level"], "info");
        assert_eq!(layers[1]["target"], "db");
        assert!(root.intercept_layers(&service("other")).is_empty());
    }

    #[test]
    fn missing_realm_names_the_service() {
        let database = service("database");
        let context = Context::root(FiberId::next());
        assert_eq!(
            context.resolve_realm(&database),
            Err(CordisError::MissingRealm { service: database })
        );
    }
}
