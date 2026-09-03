# Context

`cordis_core::Context` is a cheap, immutable, cloneable **overlay** of service realms and intercept
layers. It is not a mutable container: every scoping operation returns a *new* context whose parent
links back to the one you called it on, so children inherit their ancestor's mappings and never
mutate the parent. It is the Rust analogue of the TS `ctx` object and the realization of the
paper's recursive context `Γ∞` — a single immutable node chain in [semantics.md](../semantics.md)
§3.

Internally a `Context` is an `Arc<ContextNode>`:

```rust
struct ContextNode {
    parent: Option<Arc<ContextNode>>,
    fiber: FiberId,
    realms: BTreeMap<ServiceId, RealmId>,
    intercepts: BTreeMap<ServiceId, Arc<Value>>,
}
```

## `Context::root(fiber)`

```rust
pub fn root(fiber: FiberId) -> Self
```

Creates the root context of an application tree, bound to `fiber` and with no realms or intercepts.
Every child context shares this origin via the parent chain.

## `fiber`

```rust
pub fn fiber(&self) -> FiberId
```

Returns the `FiberId` this context belongs to. `root` binds it at construction; `extend` rebinds it.

## `extend`

```rust
#[must_use]
pub fn extend(&self, fiber: FiberId) -> Self
```

Creates an **empty overlay** belonging to `fiber`, inheriting all of the current context's realms
and intercepts. This is how a child fiber gets a context that extends its parent's: `mount_dynamic`
and the method-level inject path call `base.extend(child)` — the child shares the parent's resolver
view but is owned by a different fiber. The parent is untouched.

## `isolate`

```rust
#[must_use]
pub fn isolate(&self, service: ServiceId, realm: RealmId) -> Self
```

Creates an overlay that overrides the realm for exactly one `service`, keeping the same fiber as the
parent. Below the returned context, reads and writes of `service` resolve against `realm` instead of
the parent's mapping, so a different provider can be provided without affecting the parent scope.
Passing the same `realm` to two `isolate` calls (for the same service) joins their scopes — the
mapping is by `RealmId`, which is what makes the realm a usable grouping key.

This is the paper's `isolate` realization (Definition 25): a fresh derived context, recovered by
discarding it, with no tracked effect.

## `intercept`

```rust
#[must_use]
pub fn intercept(&self, service: ServiceId, value: Value) -> Self
```

Adds one **dynamic intercept layer** for `service` in a new overlay (same fiber as the parent). The
`value` is an arbitrary JSON value that consumers of `service` see merged into the service's
resolved config. The parent is unaffected, and your `intercept` entry is consulted only at read time
— changing it does not reload the fiber.

## `resolve_realm`

```rust
pub fn resolve_realm(&self, service: &ServiceId) -> Result<RealmId, CordisError>
```

Resolves the **nearest** realm override for `service` by walking the ancestor chain outward until it
finds a node that maps `service`.

**Errors** — returns `CordisError::MissingRealm { service }` when no overlay defines the service.

This is the first half of realm resolution. The full key is two layers:
`ProviderKey::new(service, context.resolve_realm(service)?)`, so the supervisor's provider table is
keyed by `(service, realm)`.

## `intercept_layers`

```rust
pub fn intercept_layers(&self, service: &ServiceId) -> Vec<Arc<Value>>
```

Returns the intercept `value`s for `service` from the **outermost** layer to the **innermost**
layer, i.e. ancestor-first. If no node intercepts `service`, the vector is empty.

## Behavior notes

- **Immutable overlays.** No method mutates an existing node. Two contexts sharing a parent chain
  each see exactly the mappings they created plus those they inherited.
- **Cloneable.** `Context` derives `Clone` and `Debug`; cloning is an `Arc` clone, not a deep copy.
- **Ownership vs. scope.** `extend` changes *ownership* (the fiber that will observe the context);
  `isolate` and `intercept` change *scoping* (the realm / config a service resolves in) without
  changing the fiber. `mount_dynamic` composes both: it extends to the new fiber, then isolates each
  declared (inject or provide) service into the entry's realm.

## Example

```rust
let database = ServiceId::new("database", [0; 32]);
let root_realm = RealmId::next();
let local_realm = RealmId::next();

let root = Context::root(FiberId::next()).isolate(database.clone(), root_realm);
let local = root.isolate(database.clone(), local_realm);

assert_eq!(root.resolve_realm(&database), Ok(root_realm));
assert_eq!(local.resolve_realm(&database), Ok(local_realm));
```

## Errors

- `MissingRealm { service }` — no overlay defines the service.
