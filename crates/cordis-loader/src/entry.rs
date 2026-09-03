use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type LoaderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LoaderError>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(String);

impl EntryId {
    /// Creates a stable non-empty Entry identifier.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidEntryId`] for an empty identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, LoaderError> {
        let value = value.into();
        if value.is_empty() {
            Err(LoaderError::InvalidEntryId(value))
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentRef {
    Builtin(String),
    File(String),
}

impl ComponentRef {
    /// Parses an explicit built-in or file component reference.
    ///
    /// # Errors
    ///
    /// Returns [`LoaderError::InvalidComponentRef`] for missing/unknown schemes.
    pub fn parse(value: &str) -> Result<Self, LoaderError> {
        if let Some(name) = value
            .strip_prefix("builtin:")
            .filter(|name| !name.is_empty())
        {
            Ok(Self::Builtin(name.to_owned()))
        } else if let Some(path) = value.strip_prefix("file:").filter(|path| !path.is_empty()) {
            Ok(Self::File(path.to_owned()))
        } else {
            Err(LoaderError::InvalidComponentRef(value.to_owned()))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IsolationRule {
    Local(bool),
    Global(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySpec {
    pub id: EntryId,
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub group: bool,
    #[serde(default)]
    pub intercept: BTreeMap<String, Value>,
    #[serde(default)]
    pub isolate: BTreeMap<String, IsolationRule>,
    #[serde(default)]
    pub children: Vec<Self>,
}

impl EntrySpec {
    /// Creates a leaf Entry.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is empty.
    pub fn leaf(id: impl Into<String>, component: impl Into<String>) -> Result<Self, LoaderError> {
        Ok(Self {
            id: EntryId::new(id)?,
            component: component.into(),
            config: Value::Null,
            disabled: false,
            group: false,
            intercept: BTreeMap::new(),
            isolate: BTreeMap::new(),
            children: Vec::new(),
        })
    }

    /// Creates a structural group Entry.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is empty.
    pub fn group(id: impl Into<String>, children: Vec<Self>) -> Result<Self, LoaderError> {
        Ok(Self {
            id: EntryId::new(id)?,
            component: String::new(),
            config: Value::Null,
            disabled: false,
            group: true,
            intercept: BTreeMap::new(),
            isolate: BTreeMap::new(),
            children,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedRealm {
    Local { owner: EntryId, service: String },
    Global { label: String, service: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedEntry {
    pub spec: EntrySpec,
    pub component: Option<ComponentRef>,
    pub parent: Option<EntryId>,
    pub depth: usize,
    pub effective_disabled: bool,
    pub realms: BTreeMap<String, ManagedRealm>,
    pub intercept: BTreeMap<String, Value>,
}

impl ResolvedEntry {
    pub fn is_active(&self) -> bool {
        !self.spec.group && !self.effective_disabled
    }
}

pub trait EntryDriver: Send + Sync + 'static {
    fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
    fn update<'a>(
        &'a self,
        previous: &'a ResolvedEntry,
        next: &'a ResolvedEntry,
    ) -> LoaderFuture<'a, ()>;
    fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
}

pub struct EntryTree<D> {
    driver: Arc<D>,
    roots: Vec<EntrySpec>,
    resolved: BTreeMap<EntryId, ResolvedEntry>,
    schemas: BTreeMap<String, Value>,
}

#[derive(Debug)]
enum AppliedChange {
    Stopped(ResolvedEntry),
    Updated {
        previous: ResolvedEntry,
        next: ResolvedEntry,
    },
    Started(ResolvedEntry),
}

impl<D> std::fmt::Debug for EntryTree<D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EntryTree")
            .field("roots", &self.roots)
            .field("resolved", &self.resolved)
            .field("schemas", &self.schemas.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl<D: EntryDriver> EntryTree<D> {
    pub fn new(driver: Arc<D>) -> Self {
        Self {
            driver,
            roots: Vec::new(),
            resolved: BTreeMap::new(),
            schemas: BTreeMap::new(),
        }
    }

    pub fn entries(&self) -> &BTreeMap<EntryId, ResolvedEntry> {
        &self.resolved
    }

    pub fn roots(&self) -> &[EntrySpec] {
        &self.roots
    }

    pub fn register_schema(&mut self, component: impl Into<String>, schema: Value) {
        self.schemas.insert(component.into(), schema);
    }

    /// Applies a keyed tree diff to the driver.
    ///
    /// # Errors
    ///
    /// Returns validation or driver errors without publishing the new tree. Successfully
    /// applied driver operations are rolled back in reverse order if a later operation fails.
    pub async fn reconcile(&mut self, roots: Vec<EntrySpec>) -> Result<(), LoaderError> {
        let next = resolve_entries(&roots)?;
        self.validate_configs(&next)?;

        let mut stops = self
            .resolved
            .values()
            .filter(|old| {
                old.is_active()
                    && next
                        .get(&old.spec.id)
                        .is_none_or(|new| !new.is_active() || new.component != old.component)
            })
            .cloned()
            .collect::<Vec<_>>();
        stops.sort_by_key(|entry| std::cmp::Reverse(entry.depth));
        let mut applied = Vec::new();
        for entry in &stops {
            if let Err(error) = self.driver.stop(entry).await {
                return Err(self.rollback_error(error, &applied).await);
            }
            applied.push(AppliedChange::Stopped(entry.clone()));
        }

        let mut updates = next
            .values()
            .filter_map(|new| {
                let old = self.resolved.get(&new.spec.id)?;
                (old.is_active() && new.is_active() && old.component == new.component && old != new)
                    .then(|| (old.clone(), new.clone()))
            })
            .collect::<Vec<_>>();
        updates.sort_by_key(|(_, next)| next.depth);
        for (old, new) in &updates {
            if let Err(error) = self.driver.update(old, new).await {
                return Err(self.rollback_error(error, &applied).await);
            }
            applied.push(AppliedChange::Updated {
                previous: old.clone(),
                next: new.clone(),
            });
        }

        let mut starts = next
            .values()
            .filter(|new| {
                new.is_active()
                    && self
                        .resolved
                        .get(&new.spec.id)
                        .is_none_or(|old| !old.is_active() || old.component != new.component)
            })
            .cloned()
            .collect::<Vec<_>>();
        starts.sort_by_key(|entry| entry.depth);
        for entry in &starts {
            if let Err(error) = self.driver.start(entry).await {
                return Err(self.rollback_error(error, &applied).await);
            }
            applied.push(AppliedChange::Started(entry.clone()));
        }

        self.roots = roots;
        self.resolved = next;
        Ok(())
    }

    async fn rollback_error(
        &self,
        original: LoaderError,
        applied: &[AppliedChange],
    ) -> LoaderError {
        let mut failures = Vec::new();
        for change in applied.iter().rev() {
            let result = match change {
                AppliedChange::Stopped(entry) => self.driver.start(entry).await,
                AppliedChange::Updated { previous, next } => {
                    self.driver.update(next, previous).await
                }
                AppliedChange::Started(entry) => self.driver.stop(entry).await,
            };
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            original
        } else {
            LoaderError::Driver(format!(
                "{original}; reconciliation rollback failed: {}",
                failures.join("; ")
            ))
        }
    }

    /// Inserts an Entry at the root or below a group.
    ///
    /// # Errors
    ///
    /// Returns lookup, tree validation, or driver errors.
    pub async fn create(
        &mut self,
        parent: Option<&EntryId>,
        entry: EntrySpec,
    ) -> Result<(), LoaderError> {
        let mut roots = self.roots.clone();
        if let Some(parent) = parent {
            let target = find_mut(&mut roots, parent)
                .ok_or_else(|| LoaderError::MissingEntry(parent.clone()))?;
            if !target.group {
                return Err(LoaderError::ParentNotGroup(parent.clone()));
            }
            target.children.push(entry);
        } else {
            roots.push(entry);
        }
        self.reconcile(roots).await
    }

    /// Replaces an existing Entry while preserving its position.
    ///
    /// # Errors
    ///
    /// Returns lookup, tree validation, or driver errors.
    pub async fn update(&mut self, entry: EntrySpec) -> Result<(), LoaderError> {
        let mut roots = self.roots.clone();
        let target = find_mut(&mut roots, &entry.id)
            .ok_or_else(|| LoaderError::MissingEntry(entry.id.clone()))?;
        *target = entry;
        self.reconcile(roots).await
    }

    /// Applies an Entry-originated configuration update.
    ///
    /// # Errors
    ///
    /// Returns lookup, schema validation, or driver errors.
    pub async fn self_update(&mut self, id: &EntryId, config: Value) -> Result<(), LoaderError> {
        let mut roots = self.roots.clone();
        let target =
            find_mut(&mut roots, id).ok_or_else(|| LoaderError::MissingEntry(id.clone()))?;
        target.config = config;
        self.reconcile(roots).await
    }

    /// Applies an Entry-originated disable request.
    ///
    /// # Errors
    ///
    /// Returns lookup or driver errors.
    pub async fn self_disable(&mut self, id: &EntryId) -> Result<(), LoaderError> {
        let mut roots = self.roots.clone();
        let target =
            find_mut(&mut roots, id).ok_or_else(|| LoaderError::MissingEntry(id.clone()))?;
        target.disabled = true;
        self.reconcile(roots).await
    }

    /// Moves an Entry without changing its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns lookup, invalid-parent, cycle, or driver errors.
    pub async fn move_entry(
        &mut self,
        id: &EntryId,
        parent: Option<&EntryId>,
        index: usize,
    ) -> Result<(), LoaderError> {
        if parent == Some(id) {
            return Err(LoaderError::EntryCycle(id.clone()));
        }
        let mut roots = self.roots.clone();
        let entry =
            remove_entry(&mut roots, id).ok_or_else(|| LoaderError::MissingEntry(id.clone()))?;
        if contains(&entry.children, parent) {
            return Err(LoaderError::EntryCycle(id.clone()));
        }
        let siblings = if let Some(parent) = parent {
            let target = find_mut(&mut roots, parent)
                .ok_or_else(|| LoaderError::MissingEntry(parent.clone()))?;
            if !target.group {
                return Err(LoaderError::ParentNotGroup(parent.clone()));
            }
            &mut target.children
        } else {
            &mut roots
        };
        siblings.insert(index.min(siblings.len()), entry);
        self.reconcile(roots).await
    }

    /// Removes an Entry and its descendants.
    ///
    /// # Errors
    ///
    /// Returns lookup or driver errors.
    pub async fn remove(&mut self, id: &EntryId) -> Result<(), LoaderError> {
        let mut roots = self.roots.clone();
        remove_entry(&mut roots, id).ok_or_else(|| LoaderError::MissingEntry(id.clone()))?;
        self.reconcile(roots).await
    }

    fn validate_configs(
        &self,
        entries: &BTreeMap<EntryId, ResolvedEntry>,
    ) -> Result<(), LoaderError> {
        for entry in entries.values().filter(|entry| entry.is_active()) {
            let Some(schema) = self.schemas.get(&entry.spec.component) else {
                continue;
            };
            let validator = jsonschema::draft202012::new(schema).map_err(|error| {
                LoaderError::InvalidSchema {
                    component: entry.spec.component.clone(),
                    message: error.to_string(),
                }
            })?;
            if let Err(error) = validator.validate(&entry.spec.config) {
                return Err(LoaderError::InvalidConfig {
                    entry: entry.spec.id.clone(),
                    path: error.instance_path().as_str().to_owned(),
                    message: error.to_string(),
                });
            }
        }
        Ok(())
    }
}

fn resolve_entries(roots: &[EntrySpec]) -> Result<BTreeMap<EntryId, ResolvedEntry>, LoaderError> {
    #[allow(clippy::too_many_arguments)]
    fn visit(
        entry: &EntrySpec,
        parent: Option<&EntryId>,
        depth: usize,
        parent_disabled: bool,
        parent_realms: &BTreeMap<String, ManagedRealm>,
        parent_intercept: &BTreeMap<String, Value>,
        seen: &mut BTreeSet<EntryId>,
        output: &mut BTreeMap<EntryId, ResolvedEntry>,
    ) -> Result<(), LoaderError> {
        if entry.id.as_str().is_empty() {
            return Err(LoaderError::InvalidEntryId(String::new()));
        }
        if !seen.insert(entry.id.clone()) {
            return Err(LoaderError::DuplicateEntry(entry.id.clone()));
        }
        let component = if entry.group {
            None
        } else {
            Some(ComponentRef::parse(&entry.component)?)
        };
        let mut realms = parent_realms.clone();
        for (service, rule) in &entry.isolate {
            match rule {
                IsolationRule::Local(true) => {
                    realms.insert(
                        service.clone(),
                        ManagedRealm::Local {
                            owner: entry.id.clone(),
                            service: service.clone(),
                        },
                    );
                }
                IsolationRule::Local(false) => {
                    realms.remove(service);
                }
                IsolationRule::Global(label) => {
                    realms.insert(
                        service.clone(),
                        ManagedRealm::Global {
                            label: label.clone(),
                            service: service.clone(),
                        },
                    );
                }
            }
        }
        let mut intercept = parent_intercept.clone();
        intercept.extend(entry.intercept.clone());
        let effective_disabled = parent_disabled || entry.disabled;
        output.insert(
            entry.id.clone(),
            ResolvedEntry {
                spec: entry.clone(),
                component,
                parent: parent.cloned(),
                depth,
                effective_disabled,
                realms: realms.clone(),
                intercept: intercept.clone(),
            },
        );
        for child in &entry.children {
            visit(
                child,
                Some(&entry.id),
                depth + 1,
                effective_disabled,
                &realms,
                &intercept,
                seen,
                output,
            )?;
        }
        Ok(())
    }

    let mut seen = BTreeSet::new();
    let mut output = BTreeMap::new();
    for entry in roots {
        visit(
            entry,
            None,
            0,
            false,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &mut seen,
            &mut output,
        )?;
    }
    Ok(output)
}

fn find_mut<'a>(entries: &'a mut [EntrySpec], id: &EntryId) -> Option<&'a mut EntrySpec> {
    for entry in entries {
        if &entry.id == id {
            return Some(entry);
        }
        if let Some(found) = find_mut(&mut entry.children, id) {
            return Some(found);
        }
    }
    None
}

fn remove_entry(entries: &mut Vec<EntrySpec>, id: &EntryId) -> Option<EntrySpec> {
    if let Some(index) = entries.iter().position(|entry| &entry.id == id) {
        return Some(entries.remove(index));
    }
    for entry in entries {
        if let Some(found) = remove_entry(&mut entry.children, id) {
            return Some(found);
        }
    }
    None
}

fn contains(entries: &[EntrySpec], id: Option<&EntryId>) -> bool {
    let Some(id) = id else { return false };
    entries
        .iter()
        .any(|entry| &entry.id == id || contains(&entry.children, Some(id)))
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum LoaderError {
    #[error("entry id cannot be empty")]
    InvalidEntryId(String),
    #[error("entry {0} occurs more than once")]
    DuplicateEntry(EntryId),
    #[error("entry {0} does not exist")]
    MissingEntry(EntryId),
    #[error("entry {0} is not a group")]
    ParentNotGroup(EntryId),
    #[error("moving entry {0} would create a cycle")]
    EntryCycle(EntryId),
    #[error("component reference `{0}` must use builtin: or file:")]
    InvalidComponentRef(String),
    #[error("schema for component `{component}` is invalid: {message}")]
    InvalidSchema { component: String, message: String },
    #[error("entry {entry} configuration is invalid at {path}: {message}")]
    InvalidConfig {
        entry: EntryId,
        path: String,
        message: String,
    },
    #[error("entry driver failed: {0}")]
    Driver(String),
    #[error("include failed: {0}")]
    Include(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Driver(Mutex<Vec<String>>);

    impl Driver {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    impl EntryDriver for Driver {
        fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("start:{}", entry.spec.id));
                Ok(())
            })
        }

        fn update<'a>(
            &'a self,
            _: &'a ResolvedEntry,
            next: &'a ResolvedEntry,
        ) -> LoaderFuture<'a, ()> {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("update:{}", next.spec.id));
                Ok(())
            })
        }

        fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("stop:{}", entry.spec.id));
                Ok(())
            })
        }
    }

    #[derive(Default)]
    struct FaultDriver {
        events: Mutex<Vec<String>>,
        fail_once: Mutex<Option<String>>,
    }

    impl FaultDriver {
        fn fail_once(&self, operation: &str) {
            *self.fail_once.lock().unwrap() = Some(operation.to_owned());
        }

        fn record(&self, operation: String) -> Result<(), LoaderError> {
            self.events.lock().unwrap().push(operation.clone());
            let should_fail = self.fail_once.lock().unwrap().as_deref() == Some(&operation);
            if should_fail {
                self.fail_once.lock().unwrap().take();
                Err(LoaderError::Driver(format!("injected {operation}")))
            } else {
                Ok(())
            }
        }

        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    impl EntryDriver for FaultDriver {
        fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
            Box::pin(async move { self.record(format!("start:{}", entry.spec.id)) })
        }

        fn update<'a>(
            &'a self,
            _: &'a ResolvedEntry,
            next: &'a ResolvedEntry,
        ) -> LoaderFuture<'a, ()> {
            Box::pin(
                async move { self.record(format!("update:{}:{}", next.spec.id, next.spec.config)) },
            )
        }

        fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()> {
            Box::pin(async move { self.record(format!("stop:{}", entry.spec.id)) })
        }
    }

    #[tokio::test]
    async fn keyed_reconcile_only_updates_changed_entry() {
        let driver = Arc::new(Driver::default());
        let mut tree = EntryTree::new(driver.clone());
        let a = EntrySpec::leaf("a", "builtin:a").unwrap();
        let b = EntrySpec::leaf("b", "builtin:b").unwrap();
        tree.reconcile(vec![a.clone(), b.clone()]).await.unwrap();
        assert_eq!(driver.take(), ["start:a", "start:b"]);

        let mut changed = a;
        changed.config = serde_json::json!({"value": 1});
        tree.reconcile(vec![changed, b]).await.unwrap();
        assert_eq!(driver.take(), ["update:a"]);
    }

    #[tokio::test]
    async fn group_disable_stops_children_first_and_enable_restarts_parent_order() {
        let driver = Arc::new(Driver::default());
        let mut tree = EntryTree::new(driver.clone());
        let child = EntrySpec::leaf("child", "builtin:child").unwrap();
        let mut group = EntrySpec::group("group", vec![child]).unwrap();
        tree.reconcile(vec![group.clone()]).await.unwrap();
        assert_eq!(driver.take(), ["start:child"]);
        group.disabled = true;
        tree.reconcile(vec![group.clone()]).await.unwrap();
        assert_eq!(driver.take(), ["stop:child"]);
        group.disabled = false;
        tree.reconcile(vec![group]).await.unwrap();
        assert_eq!(driver.take(), ["start:child"]);
    }

    #[tokio::test]
    async fn local_realm_survives_move_but_inherited_realm_updates_precisely() {
        let driver = Arc::new(Driver::default());
        let mut child = EntrySpec::leaf("child", "builtin:child").unwrap();
        child
            .isolate
            .insert("db".into(), IsolationRule::Local(true));
        let left = EntrySpec::group("left", vec![child]).unwrap();
        let mut right = EntrySpec::group("right", vec![]).unwrap();
        right
            .isolate
            .insert("log".into(), IsolationRule::Global("shared".into()));
        let mut tree = EntryTree::new(driver.clone());
        tree.reconcile(vec![left, right]).await.unwrap();
        driver.take();
        tree.move_entry(
            &EntryId::new("child").unwrap(),
            Some(&EntryId::new("right").unwrap()),
            0,
        )
        .await
        .unwrap();
        assert_eq!(driver.take(), ["update:child"]);
        let child = tree.entries().get(&EntryId::new("child").unwrap()).unwrap();
        assert!(
            matches!(child.realms["db"], ManagedRealm::Local { ref owner, .. } if owner.as_str() == "child")
        );
        assert!(
            matches!(child.realms["log"], ManagedRealm::Global { ref label, .. } if label == "shared")
        );
    }

    #[tokio::test]
    async fn schema_is_checked_before_start() {
        let driver = Arc::new(Driver::default());
        let mut tree = EntryTree::new(driver.clone());
        tree.register_schema("builtin:a", serde_json::json!({"type":"integer"}));
        let mut entry = EntrySpec::leaf("a", "builtin:a").unwrap();
        entry.config = Value::String("wrong".into());
        assert!(matches!(
            tree.reconcile(vec![entry]).await,
            Err(LoaderError::InvalidConfig { .. })
        ));
        assert!(driver.take().is_empty());
    }

    #[tokio::test]
    async fn failed_reconcile_rolls_back_applied_operations_in_reverse() {
        let driver = Arc::new(FaultDriver::default());
        let mut tree = EntryTree::new(driver.clone());
        let a = EntrySpec::leaf("a", "builtin:a").unwrap();
        let b = EntrySpec::leaf("b", "builtin:b").unwrap();
        tree.reconcile(vec![a.clone(), b.clone()]).await.unwrap();
        driver.take();

        let mut changed_a = a.clone();
        changed_a.config = serde_json::json!(1);
        let c = EntrySpec::leaf("c", "builtin:c").unwrap();
        driver.fail_once("start:c");
        assert_eq!(
            tree.reconcile(vec![changed_a, c]).await,
            Err(LoaderError::Driver("injected start:c".into()))
        );
        assert_eq!(
            driver.take(),
            [
                "stop:b",
                "update:a:1",
                "start:c",
                "update:a:null",
                "start:b"
            ]
        );
        assert_eq!(tree.roots(), &[a, b]);
    }
}
