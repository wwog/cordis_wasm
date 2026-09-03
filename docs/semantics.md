# Theory-to-implementation correspondence

This is the authoritative correspondence between the paper *A Programming Paradigm for
Spatiotemporal Composability* (arXiv:2608.25512, `2608.25512v1.txt` at the repo root) and the
Rust implementation. The paper formalizes Cordis; this repository is a Rust + Wasmtime rewrite of
it. The two are not byte-compatible and never promised to be — the rewrite preserves the
**semantics** (the formal model of Section 3 and the calculus of Section 4), not the TypeScript
object mechanics.

Every row below names the paper construct, its runtime counterpart in this repo, and the evidence
(file:line or the test that proves it). Where the implementation deliberately differs, the row says
so. A row marked "intentional" is a engineering decision documented here and in the README, not a
missing piece of the model.

---

## 1. Revertible effects (paper §3.1)

| Paper construct | Implementation | Evidence |
|---|---|---|
| `𝔈Γ ≔ Γ → Γ × (Γ → Γ)` — an effect returns a state plus an inverse | `Disposer` (a closure returning a future) + `EffectScope::defer` | `cordis-core/src/effect.rs:15-44`, `effect.rs:325-332` |
| `trackΓ` — compose the inverse onto the accumulator | `EffectScope::defer` pushes the disposer onto a `Vec` | `effect.rs:326-332` |
| `recoverΓ` — apply the accumulator, LIFO | `run_disposers` iterates `disposers.into_iter().rev()` | `effect.rs:413` |
| Effect iterator `ℑΓ` (§3.1.3, Definition 17/18) | `EffectGuard::spawn_stream` — each stream item is an inverse | `effect.rs:160-195` |
| LIFO recovery (Theorem 16) | `disposers_run_in_lifo_order` test | `effect.rs:493` |
| Recovery continues past a failing inverse (Theorem 16) | `failures_do_not_skip_remaining_disposers` test | `effect.rs:540` |
| Disposal at most once (paper §5.1.1: "firing twice would apply an inverse at a state no application produced") | `EffectGuard::dispose` exactly-once state machine (`Armed/Draining/Disposing/Disposed`) | `effect.rs:229-267`; `dispose_is_idempotent` test `effect.rs:477` |
| Effect ownership by the acting component | Guest `provide`/`listen` land the disposer in the current fiber's `EffectSet`; the host is authoritative for cleanup | `dynamic.rs:209` (`InstanceHost::register`), `runtime.rs:375` (`force_cleanup`) |

**Witness not checked.** The paper is explicit that `ctx.effect` does *not* verify that the
returned inverse actually reverts the effect (`𝔈*` witness): "the inverse reverts the effect it
accompanies is an obligation on the component author rather than a property the runtime verifies"
(§5.1.1). This Rust implementation likewise does not check the witness — `run_disposers` runs the
inverse but cannot confirm it restored the initial state. The two agree by design.

---

## 2. Reactive coeffects (paper §3.2)

| Paper construct | Implementation | Evidence |
|---|---|---|
| Coeffect context `Σ = (k : K) ⇀ V k` | `SupervisorState::providers: BTreeMap<ProviderKey, FiberId>` | `cordis-core/src/supervisor.rs:157` |
| `get(k)` | `resolve_dependencies` reads `state.providers` | `supervisor.rs:893-913` |
| `set(k,v)` with precondition `k ∉ dom(σ)` | `provide` rejects a `DuplicateProvider` | `supervisor.rs:748-752` |
| `withdraw` (the inverse of `set`) | `withdraw` checks owner, removes the slot | `supervisor.rs:765-791` |
| Satisfaction `σ ⊨ d` (Definition 21) | `DependencyResolution::is_ready()` + `DesiredEpoch::from_resolution` | `cordis-core/src/service.rs:122`, `fiber.rs:29-32` |
| `notify_d` classifies activating / deactivating / neutral | `recompute_affected` re-resolves only consumers whose realm matches the changed key | `supervisor.rs:915-946` |
| A component activates only when its dependencies are provided (Theorem 70) | `desired_state`: ready resolution → `Ready`, else `Waiting` | `supervisor.rs:1130-1132` |
| Provision is a revertible effect (paper §3.2.1: `set` *is* `𝔈*Σ`) | `provide` returns a `RegistryChange`; the WASM host defers the withdraw as a `Disposer` | `loader.rs:223-244` |

**Isolation and interception.** Paper §3.2.3 defines `Σiso = (K ⇀ R) × ((r:R) ⇀ V r)` and
`Σinter = ((k:K) → M k) × ((k:K) ⇀ (M k → V k))`, both realized by *derived* contexts (Definition
23): they produce a fresh context and are recovered by discarding it, with no tracked effect.

- `ContextNode { parent, fiber, realms, intercepts }` is the recursive Γ structure —
  `cordis-core/src/context.rs:12-18`.
- `isolate(service, realm)` returns a new overlay; the parent stays untouched — `context.rs:44-50`,
  matching the paper's realization (Definition 25).
- `intercept(service, value)` returns a new overlay; `intercept_layers` walks from outermost to
  innermost — `context.rs:54-60`, `76-83`.
- Realm resolution is two-layer `k ↦ ρ(k) ↦ σ(ρ(k))`: `resolve_realm` walks the ancestor chain,
  then `ProviderKey::new(service, context.resolve_realm(service)?)` — `context.rs:67-73`,
  `supervisor.rs:900-904`.

---

## 3. The context paradigm (paper §3.3)

`Γ∞ = μΓ. Γ × (Γ → Γ) × Σ` unifies the effect and coeffect contexts into one recursive type, and
its hierarchy supports nested components (the "plug-in" metaphor of §3.3.1).

| Paper construct | Implementation | Evidence |
|---|---|---|
| `Γ∞` recursive context | `ContextNode { parent, fiber, realms, intercepts }` | `context.rs:12-18` |
| Derived child context (Definition 23) | `Context::extend` — an immutable overlay bound to a child fiber | `context.rs:38-40` |
| Hierarchical composition | `InstanceHost` carries a `Context`; `mount_dynamic` extends the parent context | `dynamic.rs:150-164`, `dynamic.rs:570-571` |

**`≃` (observational equivalence), the one non-formalized piece.** Paper §3.3.2 and §4.3.2 read the
recovery guarantee "up to `≃`": a free allocator is not rewound, a message already sent stays sent.
This implementation does not carry a type-level `≃`; it achieves the same effect structurally —
external, non-revertible resources (logs, sent messages, cross-process I/O) are never "restored",
because their acquisition (a `Disposer`) and their emission (an outbound call) are separate, and
the emission is the paper's "acts as `idΓ`". The guarantee holds semantically, but there is no
formal `≃` type in the code.

---

## 4. The calculus (paper §4)

### 4.1 Fiber state machine

| Paper state (§4.1, Figure 1) | Rust `FiberState` |
|---|---|
| `Inactive` | `Pending` |
| `Reloading` | `Loading` |
| `Active` | `Active` |
| `Unloading` | `Unloading` |
| (failure extension, §4.4) | `Failed`, `Disposed` |

### 4.2 The nine rules

| Rule | Implementation | Evidence |
|---|---|---|
| `O-Insert` | `CreateFiber` command | `supervisor.rs:562-578`, `create_fiber` `supervisor.rs:636` |
| `O-Retire` | `RetireFiber` command | `supervisor.rs:839-852` |
| `O-Remove` | fiber removed from `state.fibers` after `Disposed` | `supervisor.rs:656-677` |
| `L-Begin` | `set_desired(Ready)` starts a `Load` | `fiber.rs:132-136` |
| `L-Iter` / `L-Finish` | `run_dynamic_transition` `Load` branch runs `activate` | `dynamic.rs:631-672` |
| `L-Leave` | `set_desired(Unload)` starts an `Unload` | `fiber.rs:146` |
| `L-Divert` | `reload` chaining (`replace` → `reload_fiber`) | `fiber.rs:194-203`, `dynamic.rs:435-474` |
| `L-Unload` (guarded) | `schedule_transition_batch` blocks unloads, releases when no consumers | `supervisor.rs:1068-1086`, `release_ready_unloads` `supervisor.rs:1104-1115` |

### 4.3 The guard (the core of spatial composability)

Paper §4.2.2: `relied_n(γ)` holds when some installed fiber resolves a key to `n`; `L-Unload`
requires `¬relied_n(γ)`. This is what makes a provider withdraw only *after* its dependents have
gone (Theorem 70).

- `has_active_consumers` is `relied_n`: it tests whether any `Loading/Active/Unloading` fiber has a
  committed view naming the provider — `supervisor.rs:1117-1128`.
- `release_ready_unloads` is the guard: blocked unloads are released only when `!has_active_consumers`
  — `supervisor.rs:1104-1115`.
- Teardown order consumer-first → `teardown_drains_consumers_before_providers` test:
  `provider -> middle -> leaf`; retiring the provider tears down `leaf` first, then `middle`, then
  `provider` — `supervisor.rs:1453`.

### 4.4 Target vs committed view

Paper distinguishes `ω_n` (committed view, the resolution the fiber activated against) from
`target_n(γ)` (the resolution it *should* run against); a transition fires on their differing.

- `desired` (`DesiredEpoch`) is the target view; `committed` (`CommittedView`) is `ω_n` —
  `supervisor.rs:698-708`, `commit_dependencies` `supervisor.rs:727-738`.
- The view records the **provider** (`EpochEntry { key, provider: Option<FiberId> }`,
  `fiber.rs:17-20`), not the value — exactly the paper's "recording a provider rather than a value".
- Provider identity is `FiberId`, fresh and never reused, so a replaced provider is never mistaken
  for its predecessor even when the values are equal (`fiber.rs:395-397`).

### 4.5 Dependency cycles

Paper §6.5: "a dependency cycle simply leaves the involved components permanently inactive... this
condition is predictable from the dependency declarations alone, so a runtime can report it."

- `dependency_cycles` runs an SCC over the provider–consumer graph and marks each cycle member
  `Waiting` with a `DependencyCycle` error — `supervisor.rs:979-1059`, `963-967`.
- Test `dependency_cycle_reports_every_scc_member` — `supervisor.rs:1404`.

---

## 5. Implementation (paper §5)

### 5.1.3 Component lifecycle (Algorithm 5)

`refresh` → recompute target; if not already in a transition, start `reload` or `unload`. `reload`
commits the view, runs the effect function, and checks the target at completion: if it still
matches, enter `Active`; otherwise chain into `unload`. `unload` reverts effects LIFO, waits for
dependents (the guard), then either `Inactive` or chains into `reload`.

| Algorithm 5 line | Implementation |
|---|---|
| `fiber.target ← target` | `record.snapshot.desired` updated by `configure_dependencies` / `recompute_affected` |
| `fiber.committed ← resolve(inject)` | `commit_dependencies` freezes the ready resolution — `supervisor.rs:727-738` |
| `recover ← await execute(fiber.apply, guard)` | `run_dynamic_transition` `Load` branch runs `activate` — `dynamic.rs:631-672` |
| `if fiber.target = target0 then ACTIVE else UNLOADING` | `complete_load` checks the desired epoch is current, else chains an `Unload` — `fiber.rs:205-225` |
| `unload ... await all(notify(...))` | blocked unload waits for consumers — `supervisor.rs:1068-1086` |
| Inertia (once a transition begins it completes) | `FiberMachine` runs a transition to completion; a target change during a transition coalesces and chains — `fiber.rs:125-149`, `fiber.rs:299-328` |

### 5.2.1 Declarative configuration and reconciliation

| Paper construct | Implementation | Evidence |
|---|---|---|
| Entry records `id, url, isolate, intercept, config, disabled` (§5.2.1, Definition 81) | `EntrySpec { id, component, config, disabled, group, intercept, isolate, children }` | `cordis-loader/src/entry.rs:75-91` |
| Keyed reconciliation over `id` | `EntryTree::reconcile`: stop (depth-descending), update, start | `entry.rs:224-289` |
| `@cordisjs/group` nested loading | `group: bool` + `children` | `entry.rs:117-128` |
| `@cordisjs/include` external YAML/JSON | `cordis-loader/src/include.rs` | — |
| `isolate` local / global realms (paper Algorithm 7) | `ManagedRealm::Local` / `Global`, `realm_for` | `entry.rs:131-135`, `loader.rs:514-535` |
| `intercept` updated in place, no reload | `intercept` metadata consulted at read time | `context.rs:76-83` |
| Transactionality (paper §5.2.1: reconcile sound by Theorem 80) | `rollback_error` reverses applied operations | `entry.rs:291-317`; test `failed_reconcile_rolls_back_applied_operations_in_reverse` |

### 5.2.2 Hot module replacement

| Phase | Implementation |
|---|---|
| Preflight (candidate compile / descriptor / WIT / capability checks *before* touching an instance) | `reload_paths` compiles and checks every candidate, returns a preflight failure report on any error — `hmr.rs:352-375` |
| Transactional replace | `commit_candidates` replaces each entry; on failure, rolls back all already-replaced entries in `attempted.into_iter().rev()` — `hmr.rs:425-510` |
| Backup / restore (paper Algorithm 10) | `restore` re-applies the previous artifact — `hmr.rs:216-218` |

**Intentional difference.** Paper HMR classifies a *module import graph* (`get_imports`,
`get_dependencies`, Webpack/Vite acceptance boundaries). Dynamic code here is a single Wasmtime
Component, so there is no JS module graph: HMR reduces to "artifact content hash changed → replace
that fiber". The semantic core survives — HMR is still fiber replacement plus transactional
rollback, and it still needs no developer-annotated acceptance boundary (paper §5.2.2's central
claim), because the fiber already bounds the component's effects.

---

## 6. Discussion (paper §6)

| Paper section | Implementation |
|---|---|
| §6.1 System boundary: acquisition (revertible) vs emission (acts as `idΓ`) | WASM `provide_service`/`register_listener` return a `Resource<Registration>` the host holds (acquisition); outbound `call_service` payloads are not reverted (emission). `force_cleanup` clears registrations — `runtime.rs:375-385` |
| §6.2 Service multiplexing: exclusive binding or broker | Exclusive binding via `DuplicateProvider`; the *realms* route (paper's other option) via `ServiceId + RealmId` — `supervisor.rs:748` |
| §6.3 Capability-based access control | A component accesses only what it declared: `call_service` goes through the committed view and rejects undeclared/missing providers — `loader.rs:195-205` |
| §6.3 Sandboxing | Wasmtime Component Model: the guest reaches the host only through the WIT kernel interface; `capabilities` in the manifest are checked against `ArtifactPolicy` — `dynamic.rs:39`, `hmr.rs:33` |
| §6.4 Language independence | Closures (`Disposer`), dynamic module registry (Wasmtime Component), typed dependency access (`DependencySet` trait + `#[cordis::service]` macro), compile-time metaprogramming for accessors (paper's stated Rust path) — `native.rs:240` |
| §6.5 Mutual dependencies | SCC cycle detection — `supervisor.rs:979-1059` |
| §6.6 Key collision | `ServiceId` carries an ABI hash (name + method + arg types + return type); this is the paper's key-namespacing route — `service.rs:8-31` |

---

## 7. Explicit, intentional deviations

These are not gaps in the model; each is a deliberate engineering choice documented in the README
and here.

1. **Wasm HMR has no module import graph.** Paper HMR traverses JS imports; the rewrite replaces
   single Wasmtime Components. Core semantics (fiber replacement + transaction) preserved.
2. **Business service protocol is MessagePack over the Kernel WIT**, not TS objects. The README
   states "does not promise binary compatibility with TS plugins".
3. **Effect ownership enforcement differs by path.** Native path uses macro-generated adapters;
   WASM path relies on the host as final authority (the README's "host effect table is final
   authority").
4. **`ReentrantCall` guard.** `CallGate` rejects same-fiber reentrancy to avoid a Wasmtime Store
   deadlock (`dynamic.rs:325-348`). The paper discusses inertia (§4.4) but not re-entrancy. This is
   a restrictive-but-necessary addition, recorded in `docs/wasmtime-findings.md`.
5. **Effect witness not runtime-verified** — matches the paper (§5.1.1); not a deviation.

---

## 8. Verification status

Every semantic mechanism listed above has a test in the repo. The following map the paper's core
theorems to their tests:

| Paper theorem / claim | Test |
|---|---|
| Theorem 16 (LIFO recovery) | `disposers_run_in_lifo_order` |
| Theorem 70 (ordering: provider outlives consumer) | `teardown_drains_consumers_before_providers` |
| Theorem 73 (progress / no deadlock) | `generated_transition_sequences_preserve_state_invariants` |
| §6.5 (cycle predictability) | `dependency_cycle_reports_every_scc_member` |
| §5.2.1 (transactional reconcile) | `failed_reconcile_rolls_back_applied_operations_in_reverse` |
| §5.2.2 (transactional HMR) | `apply_failure_rolls_back_failed_and_prior_entries_in_reverse_order` |
| Theorem 80 (confluence: quiescence is a function of the final configuration, not the schedule) | `quiescent_state_is_a_function_of_the_final_configuration_not_the_schedule` |
