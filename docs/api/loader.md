# Loader (Declarative Entries)

`cordis-loader` is the declarative side of Cordis. An application is described as a tree of
**Entries** — a `cordis.json`/YAML document (or an include chain) that names which components to run,
with what config, under which isolation and intercept rules. The loader resolves that tree, validates
configs against registered schemas, and **reconciles** it against the current state by keyed diff,
driving a driver that actually mounts/stops/updates components.

this maps to the paper's §5.2.1 declarative configuration: an entry records `id, url, isolate,
intercept, config, disabled` (Definition 81), and reconcile is sound by Theorem 80.

The file syntax itself — what you can write in a `cordis.json`/YAML, how `isolate` and `intercept`
behave, and the `!expr` Rhai dynamic configuration — is documented in [config](config.md).

## `EntryId`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(String);
impl EntryId {
    pub fn new(value: impl Into<String>) -> Result<Self, LoaderError>;
    pub fn as_str(&self) -> &str;
}
```

A stable non-empty Entry identifier. This is the **key** for reconciliation: an entry's identity is
its `id`, not its position.

**Errors** — `new` returns `LoaderError::InvalidEntryId(value)` for an empty identifier.

## `ComponentRef`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentRef {
    Builtin(String),
    File(String),
}
impl ComponentRef {
    pub fn parse(value: &str) -> Result<Self, LoaderError>;
}
```

A component reference in an entry. Two schemes:

- `builtin:<name>` — a process-local factory registered with the `BuiltinRegistry` (see
  [wasm-driver](wasm-driver.md)).
- `file:<path>` — a `.wasm` component on disk, resolved against the application base directory.

**Errors** — `parse` returns `LoaderError::InvalidComponentRef` for a missing/unknown scheme. Bare
names (no scheme) are rejected; the scheme is required.

## `IsolationRule`

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IsolationRule {
    Local(bool),
    Global(String),
}
```

Isolation config for one named service in an entry's `isolate` map. `Local(true)` creates a fresh
local realm owned by the entry (overriding any inherited realm); `Local(false)` removes the inherited
realm (revert to default); `Global(label)` joins a realm shared across entries that use the same
`label`.

## `EntrySpec`

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySpec {
    pub id: EntryId,
    #[serde(default)] pub component: String,
    #[serde(default)] pub config: Value,
    #[serde(default)] pub disabled: bool,
    #[serde(default)] pub group: bool,
    #[serde(default)] pub intercept: BTreeMap<String, Value>,
    #[serde(default)] pub isolate: BTreeMap<String, IsolationRule>,
    #[serde(default)] pub children: Vec<Self>,
}
```

One entry in the declarative tree. `group: true` makes it a structural container (no component, only
`children`). `disabled` is inherited by children (`effective_disabled`). `intercept` is a
service-name → JSON-value map merged into the service's config for entries below this point.

Constructors:

```rust
impl EntrySpec {
    pub fn leaf(id: impl Into<String>, component: impl Into<String>) -> Result<Self, LoaderError>;
    pub fn group(id: impl Into<String>, children: Vec<Self>) -> Result<Self, LoaderError>;
}
```

**Errors** — both return an error when `id` is empty.

## `ManagedRealm`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedRealm {
    Local { owner: EntryId, service: String },
    Global { label: String, service: String },
}
```

The realm a resolved entry uses for a service, after resolving local/global isolation rules. `Local`
bounds the realm to the owning entry; `Global` bounds it to a label shared by entries that select the
same group.

## `ResolvedEntry`

```rust
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
    pub fn is_active(&self) -> bool { !self.spec.group && !self.effective_disabled }
}
```

A fully resolved entry: its spec, parsed component ref (or `None` for groups), parent id, depth,
effective-disabled flag, and the merged realms/intercepts inherited from ancestors.

## `EntryDriver`

```rust
pub type LoaderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LoaderError>> + Send + 'a>>;

pub trait EntryDriver: Send + Sync + 'static {
    fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
    fn update<'a>(&'a self, previous: &'a ResolvedEntry, next: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
    fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
}
```

Executes one Entry operation atomically. **A method returning an error must leave the runtime in its
pre-call state** — `EntryTree` uses that guarantee to roll back earlier successful operations when a
later one fails. Implementations: `WasmEntryDriver` (see [wasm-driver](wasm-driver.md)) and
`PreflightDriver` (built into `check_entries`).

## `EntryTree<D>`

```rust
pub struct EntryTree<D> { /* driver: Arc<D>, roots, resolved, schemas */ }
impl<D: EntryDriver> EntryTree<D> {
    pub fn new(driver: Arc<D>) -> Self;
    pub fn entries(&self) -> &BTreeMap<EntryId, ResolvedEntry>;
    pub fn roots(&self) -> &[EntrySpec];
    pub fn register_schema(&mut self, component: impl Into<String>, schema: Value);

    pub async fn reconcile(&mut self, roots: Vec<EntrySpec>) -> Result<(), LoaderError>;
    pub async fn create(&mut self, parent: Option<&EntryId>, entry: EntrySpec) -> Result<(), LoaderError>;
    pub async fn update(&mut self, entry: EntrySpec) -> Result<(), LoaderError>;
    pub async fn self_update(&mut self, id: &EntryId, config: Value) -> Result<(), LoaderError>;
    pub async fn self_disable(&mut self, id: &EntryId) -> Result<(), LoaderError>;
    pub async fn move_entry(&mut self, id: &EntryId, parent: Option<&EntryId>, index: usize) -> Result<(), LoaderError>;
    pub async fn remove(&mut self, id: &EntryId) -> Result<(), LoaderError>;
}
```

Owns the current entry tree and reconciles it against a target.

### `reconcile` (keyed diff)

Applies a keyed tree diff to the driver. The algorithm is stop → update → start:

1. **Resolve** the target tree (`resolve_entries`), checking for duplicate/empty ids, then **validate
   configs** against registered schemas *before any driver call*.
2. **Stop** entries that are no longer active or whose component changed, depth-**descending** (deepest
   first, so children stop before their parents).
3. **Update** entries that stayed active with the same component but changed in some other way,
   depth-ascending.
4. **Start** new entries (or re-enabled ones), depth-ascending (parents before children).

**Errors** — returns a validation or driver error **without publishing the new tree**. Successfully
applied driver operations are rolled back in reverse order if a later operation fails
(`rollback_error`), so a failed reconcile leaves the runtime in its pre-call state. Test:
`failed_reconcile_rolls_back_applied_operations_in_reverse`.

### Other operations

- `create(parent, entry)` — insert at the root or below a `group`. `MissingEntry`,
  `ParentNotGroup`.
- `update(entry)` — replace an existing entry preserving its position. `MissingEntry`.
- `self_update(id, config)` — apply a config update originating from the entry itself.
- `self_disable(id)` — disable an entry.
- `move_entry(id, parent, index)` — move an entry without changing its stable id. `EntryCycle` if the
  move would create a cycle, `ParentNotGroup`.
- `remove(id)` — remove an entry and its descendants. `MissingEntry`.
- `register_schema(component, schema)` — register a JSON Schema for a component, used by
  `validate_configs`.

## `LoaderError`

```rust
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum LoaderError {
    InvalidEntryId(String),
    DuplicateEntry(EntryId),
    MissingEntry(EntryId),
    ParentNotGroup(EntryId),
    EntryCycle(EntryId),
    InvalidComponentRef(String),
    InvalidSchema { component: String, message: String },
    InvalidConfig { entry: EntryId, path: String, message: String },
    Driver(String),
    Include(String),
}
```

## `IncludeDocument` and config includes

`include.rs` adds `@cordisjs/include`-style external config files. An include document is loaded from
JSON or YAML, evaluated (YAML `!expr` tags run a restricted Rhai expression against the current `ctx`
snapshot), patched, and optionally written back atomically.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeFormat { Json, Yaml }
impl IncludeFormat {
    pub fn from_path(path: &Path) -> Result<Self, LoaderError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Patch { pub target: Option<EntryId>, pub action: PatchAction }

#[derive(Clone, Debug, PartialEq)]
pub enum PatchAction {
    Merge(Value),
    Replace(EntrySpec),
    Remove,
    Insert { index: usize, entry: EntrySpec },
}

#[derive(Debug)]
pub struct IncludeDocument { /* path, format, readonly, entries */ }
impl IncludeDocument {
    pub fn load(path: impl Into<PathBuf>, patches: &[Patch], expression_context: &Value) -> Result<Self, LoaderError>;
    pub fn from_source(path, format, readonly, source, patches, expression_context) -> Result<Self, LoaderError>;
    pub fn entries(&self) -> &[EntrySpec];
    pub fn entries_mut(&mut self) -> &mut Vec<EntrySpec>;
    pub fn readonly(&self) -> bool;
    pub fn write_back(&self) -> Result<(), LoaderError>;
}
```

- `load` reads a file, infers the format from the extension, and evaluates the patches.
- YAML `!expr` tags evaluate against the `ctx` JSON snapshot in a **restricted** Rhai engine
  (`eval`, `import`, `export`, `fn`, `while`, `loop`, `for`, `try`, `throw` are disabled; operation
  and size budgets are capped).
- `write_back` atomically rewrites the materialized entry array (temp file + rename), or fails if the
  document is read-only (mode `0o222` unset on Unix).

**Errors** — `load`/`from_source`: filesystem, syntax, expression, or patch errors.
`write_back`: read-only document or failed filesystem operation.

## `ExprEvaluator`

```rust
#[derive(Default)]
pub struct ExprEvaluator { /* engine: rhai::Engine */ }
impl ExprEvaluator {
    pub fn evaluate(&self, expression: &str, context: &Value) -> Result<Value, LoaderError>;
}
```

Evaluates one restricted Rhai expression against the JSON `ctx` snapshot, returning a JSON value.

**Errors** — forbidden syntax, budget exhaustion, or non-JSON result.

## Entry format for `cordis check`

The CLI's `load_entries` reads a `cordis.json`/YAML document whose root is either an entry array or an
object with an `entries` array, then feeds the entries to `check_entries`/`reconcile`. The `CORDIS_SHARED`
environment variable supplies the `ctx` value for `!expr` in YAML includes (see [cli](cli.md)).
