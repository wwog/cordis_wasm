# Application Configuration (`config.{json,yaml}`)

The CLI and the loader take a single **declarative config file** that describes the whole
application: which components to run, with what config, under which isolation and intercept rules,
and how they nest. The file is JSON or YAML and its root must be a list of entries or an object
with an `entries` key. `cordis check`, `cordis run`, and `cordis inspect` all accept it
(`common/config`):

```sh
cordis check   examples/wasm-app/cordis.json
cordis run     examples/wasm-app/cordis.json
cordis inspect examples/wasm-app/cordis.yaml
```

The loader side is `IncludeDocument` ([loader](loader.md)), which loads the file and materializes a
`Vec<EntrySpec>`; the reconcile step then drives the entry tree. This page documents **the file
syntax itself** — fields allowed on each entry, the root shape, includes, patches, and the YAML
`!expr` dynamic configuration.

## Root shape

The file must be either a **plain array** of entry objects, or an object whose `entries` field is
that array (which lets you carry extra top-level metadata):

```yaml
# Either form is accepted:
- id: consumer
  component: file:../target/wasm32-wasip2/debug/app.wasm

# Or:
entries:
  - id: consumer
    component: file:../target/wasm32-wasip2/debug/app.wasm
```

Anything else is rejected with `LoaderError::Include("include root must be an entry array or
contain `entries`")`.

## Entry fields

An **entry** is an `EntrySpec`. The fields, with the serde rename to `camelCase`:

```rust
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
```

| Field | Type | Meaning |
|---|---|---|
| `id` | string | Stable identity for this entry — the **key** for reconciliation. Required; non-empty. An edit to an existing entry is recognized by unchanged `id`, so the loader applies an update instead of remove-plus-add. |
| `component` | string | The component reference, `builtin:<name>` or `file:<path>` (see `ComponentRef` below). Required for leaf entries, empty for `group` entries. |
| `config` | object | The plugin's config value, passed to its `activate`. Validated against the component's declared JSON Schema (`config_schema`) before loading. |
| `disabled` | bool | Keep the entry but skip mounting it. `true` unmounts the plugin and everything waiting on its services; `false` re-mounts. |
| `group` | bool | Make this a structural **group** that nests `children` and loads/unloads them as one unit. A group entry carries no `component`. |
| `intercept` | object | Service-specific config intercept: `service-name → config-value`. Merged into the resolved config for plugins loaded below this entry (ancestor entries first). Consulted at read time — changing `intercept` does **not** reload the plugin. |
| `isolate` | object | Service isolation rule: `service-name → rule` (see `IsolationRule` below). Gives an entry its own instance/scoping of a service name. |
| `children` | array | For `group` entries, the nested sub-list. |

Because every field is `#[serde(default)]` except `id`, an entry can be a leaf with just `id` +
`component`. An empty or duplicate `id`, a `component` on a group, or a duplicate leaf id in the
same tree is a validation error.

### `ComponentRef`

`component` must use a scheme — a bare path is rejected:

- `builtin:<name>` — a process-local factory registered with `BuiltinRegistry` (see
  [wasm-driver](wasm-driver.md) and the wasm host).
- `file:<path>` — a `.wasm` component on disk, resolved against the application's base directory
  (the config file's parent). The path is relative to that base.

### `IsolationRule`

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IsolationRule {
    Local(bool),
    Global(String),
}
```

| YAML form | YAML value | Meaning |
|---|---|---|
| `service: true` | `Local(true)` | Enter a **local** realm: the entry and its descendants read/write `service` against a realm owned by this entry. |
| `service: false` | `Local(false)` | Leave the inherited realm for `service` (falls back to the nearest ancestor realm). |
| `service: <label>` | `Global(label)` | Enter a **global** shared realm keyed by `label`; two entries with the same label and service share one realm. |

Realm scoping is two layers (`k ↦ ρ(k) ↦ σ(ρ(k))`): the entry tree decides which **realm** each
service resolves in, and the supervisor decides which provider fills that realm. The reference
`examples/wasm-app/cordis.json` uses this so the provider and consumer see the **same** `example`
realm for `example.counter`:

```json
{
  "entries": [
    { "id": "consumer", "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_consumer.wasm",
      "config": {}, "isolate": { "example.counter": "example" } },
    { "id": "provider", "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_provider.wasm",
      "config": {}, "isolate": { "example.counter": "example" } }
  ]
}
```

Here both entries pin `example.counter` to the global realm `example`, so the consumer's `inject`
resolves to the provider's `provide`.

## The `config` value and JSON Schema

`config` is any JSON value. Before the entry is started, it is validated against the component's
**config schema** — the `config_schema` JSON in the component's descriptor, parsed as JSON Schema
Draft 2020-12:

- The loader (`EntryTree::validate_configs` and the driver's `validate_config`) compiles it with
  `jsonschema::draft202012` and runs it against `entry.config`.
- A mismatch fails the whole reconcile with `LoaderError::InvalidConfig` naming the entry, the
  failing JSON-Pointer path, and the message. An invalid schema fails with `InvalidSchema`.
- The guests in `examples/` declare the strict schema `{"type":"object","additionalProperties":false}`
  so only `config: {}` passes; an unknown key is rejected.
- `true`/`false` are valid schemas in Draft 2020-12: `true` accepts anything, `false` nothing.

So `config` is not free-form: what a plugin accepts is bounded by its `config_schema`, and a bad
value is a **loud** failure at preflight/load, not a silent skip.

## Includes and patches

A config file can pull in and reshape other files. `IncludeDocument::load(path, patches, context)`
reads one file and applies a list of [`Patch`]es before materializing the entries. Each patch names
a `target` `EntryId` and an action:

| Action | Behavior |
|---|---|
| `Merge(value)` | Deep-merge `value` into the target entry's JSON, then re-parse as `EntrySpec`. The merged `id` must equal the target. |
| `Replace(entry)` | Replace the target entry with `entry`; `entry.id` must equal the target. |
| `Remove` | Remove the target entry (and its children). |
| `Insert { index, entry }` | Insert `entry` at `index` into the target's `children` (or the root list if no target). The target must be a group. |

`PatchAction::Merge` uses recursive deep merge: object values merge key-by-key, and any non-object
value overwrites.

## Rhai dynamic configuration (`!expr`)

**YAML only.** In a YAML config, a value may be a tagged scalar `!expr <expression>` whose result
replaces the literal. The expression is evaluated by a **restricted** Rhai engine against a `ctx`
snapshot bound into the scope as the variable `ctx`.

```yaml
- id: app
  component: builtin:test
  config:
    port: !expr ctx.port + 1
```

With `ctx = {"port": 40}`, `config.port` becomes `41` (this exact case is covered by the loader test
`yaml_expr_is_evaluated_recursively_in_restricted_scope`).

### Where `ctx` comes from

The snapshot is the JSON `CORDIS_SHARED` environment variable (see [cli](cli.md)); if unset it is
`null`. It is the runtime's view of shared application state, surfaced during **load** so the config
itself can be a function of it.

### Restriction

The `ExprEvaluator` engine is deliberately sandboxed:

- Evaluation budgets are set: `10_000` max operations, max expression depth `32`, max string size
  `64 KiB`, max array/map size `10_000`.
- A fixed set of symbols is **disabled**, so the usual escape hatches are gone:
  `eval`, `import`, `export`, `fn`, `while`, `loop`, `for`, `try`, `throw`.
- The result must round-trip to a JSON value (`rhai::serde::from_dynamic`); a non-JSON result errors.
- The tag is evaluated **recursively** at any depth: a `!expr` that appears bool, in a sequence, or
  in a nested mapping is evaluated there.

```yaml
- id: "app"
  component: builtin:test
  config:
    replicas: !expr ctx.replicas * 2
    tags:
      - !expr ctx.environment        # a `!expr` inside a sequence is evaluated too
- id: "other"
  component: builtin:test
  config: !expr ctx.maybe_null       # even a whole `config` value
```

Only the `!expr` tag is recognized; any other tag (`!danger`, an arbitrary `!foo`) is rejected with
`LoaderError::Include("unsupported YAML tag ...")`. A `!expr` whose value is not a scalar string is
rejected. And in **JSON** there is no tag syntax at all — dynamic values are not available; use YAML
for `!expr`.

## Read-only and atomic write-back

`IncludeDocument` records whether the source file is known read-only (on Unix, no `0o222` write
bits). Its `write_back()` materializes the entry array back to the source — JSON pretty-printed or
YAML — via an atomic temp-file rename, and refuses a read-only document:
`LoaderError::Include("include ... is read-only")`. The loader uses this for self-updates and
originated changes, so a config a plugin wants to mutate must be writable.

## Failure modes

| Symptom | Cause |
|---|---|
| `InvalidComponentRef` | `component` has no `builtin:`/`file:` scheme, or a bare path was used. |
| `InvalidConfig` | `config` does not satisfy the component's `config_schema` (Draft 2020-12). Names the entry, JSON-Pointer path, and message. |
| `InvalidSchema` | The component's `config_schema` is not a valid JSON Schema object/boolean. |
| `Include: unsupported YAML tag` | A tag other than `!expr` was used, or a `!expr` was not a scalar string. |
| `!expr ... failed` | The Rhai expression errored, hit an evaluation budget, used a disabled symbol, or returned a non-JSON value. |
| `DuplicateEntry` / `InvalidEntryId` | Two entries share an `id`, or an `id` is empty. |
| `ParentNotGroup` | An `isolate`/`intercept`/patch targeted a non-group as a parent, or a leaf was used as a group. |

## See also

- [loader](loader.md) — `EntrySpec`, `EntryId`, `EntryTree::reconcile`, the loader error enum.
- [wasm-driver](wasm-driver.md) — how a loaded entry tree is mounted onto Wasmtime fibers.
- [cli](cli.md) — the `cordis` subcommands and `CORDIS_SHARED`.
- [macros](macros.md) — how a component's `#[cordis::component(config = ...)]` produces its `config_schema`.
