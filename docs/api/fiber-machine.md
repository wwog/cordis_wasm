# Fiber Machine Reference

A deeper look at the `FiberMachine` invariant and the behavior that separates "a state machine that
happens to work" from one that provably preserves its invariants. [fiber](fiber.md) covers the public
surface and the semantics; this page goes into the internal proof and the exact chaining protocol.

The machine is deliberately incomplete on its own: it only *records* that work is in flight
(`active: Option<FiberTransition>`) and what work to do next (`TransitionAdvance::Start`). The
[supervisor](supervisor.md) is the single writer that actually runs the transition and reports back
through `complete`. That split — pure decision machine, external executor — is what makes the
invariant testable by exhaustion.

## The invariant

The core invariant, asserted on every step of the generated test:

```rust
let transitioning = matches!(machine.state(), FiberState::Loading | FiberState::Unloading);
assert_eq!(machine.active_transition().is_some(), transitioning);
if disposed { assert_eq!(machine.state(), FiberState::Disposed); }
```

In words:

1. **A transition is in flight iff the state is `Loading` or `Unloading`.** The machine never has a
   dangling `active` transition (the executor would wait forever on a completion), and never reports
   a lifecycle state that implies work without an `active` record.
2. **`Disposed` is absorbing.** Once `Disposed`, no input makes it leave.

This is the `transitioning` assertion in
`generated_transition_sequences_preserve_state_invariants` (`fiber.rs:407-464`). The test drives
128 seeds, each 256 steps, mixing every input (`set_desired` to `Waiting`/`Ready`/`Retired`,
`restart`, `reload`, `complete` with both a current and a saturating+1 generation, and both `Ok` and
`Err` results), and checks the invariant after *every* step. Since the input space is small and the
state space is small, this is an exhaustive sweep of reachable behavior.

## Transition coalescing

A target change during a transition does not start a second one. `set_desired`:

```rust
pub fn set_desired(&mut self, desired: DesiredState) -> Option<FiberTransition> {
    self.desired = desired;
    if self.active.is_some() {
        return None;   // coalesce: remember the target, do nothing now
    }
    ...
}
```

The supervisor learns about the change only when the in-flight transition completes. This is the
mechanism behind the paper's inertia: you cannot interrupt a running `Load`; you can only record a
new target that the machine will observe at the next `complete`.

Coalescing is not "last write wins at the cost of correctness": because `complete` re-checks the
*current* desired against the *just-completed* epoch, the final state always reflects the latest
target, regardless of how many intermediate targets were skipped. Test:
`desired_changes_coalesce_during_load` drives `Ready(first)` → `set_desired(Ready(second))` (gets
`None`) → `complete(first, Ok)` → expects a chained `Unload` → `complete(unload, Ok)` → expects a
chained `Load(second)`. The skipped `first` epoch was never activated.

## Chaining, not interruption

When a transition completes and the target no longer matches, the machine returns
`TransitionAdvance::Start(next)` instead of settling. The two chain sites:

- **`complete_load`**: `is_current` compares `desired` to the just-loaded epoch. If they differ, it
  records `loaded_epoch` and returns `Start(start(Unload))`. (The `Unload` then re-checks the same
  `desired` on its own completion and chains back to a `Load` if it is still `Ready`.)
- **`complete_unload`**: on `Ready(epoch)` returns `Start(start(Load { epoch }))`; on `Waiting`
  settles to `Pending`; on `Retired` settles to `Disposed`.

So a single *compound* change (e.g. `Ready(A)` → `Ready(B)` while loading `A`) becomes an `Unload`
chain followed by a `Load(B)`. The chain is always one transition at a time — the machine never
executes two transitions in one call, and never aborts a transition to substitute a new one.

## The generation counter

Each `start` stamps the transition with `next_generation` and increments it. `complete` ignores any
generation that does not equal the active transition's generation:

```rust
if active.generation != generation { return TransitionAdvance::IgnoredStale; }
```

This makes a late completion harmless. Consider: a `Load` completes, the machine chains an `Unload`,
and a *stale* `Load` completion (from a duplicate report, or a delayed task) arrives afterward. It is
`IgnoredStale` — it cannot clobber the `Unload`'s active transition or change state. Test:
`stale_completion_cannot_change_current_transition` completes generation + 1 and asserts the active
transition is unchanged.

## The load/unload completion matrix

| Completed | Result | Guard | Outcome |
|---|---|---|---|
| `Load { epoch }` | `Ok` | `desired == Ready(epoch)` | state → `Active`; `Settled` |
| `Load { epoch }` | `Ok` | `desired != Ready(epoch)` | record `loaded_epoch`; `Start(Unload)` |
| `Load { epoch }` | `Err` | — | `failure = Some(e)`; state → `Failed`; `Settled` |
| `Unload` | `Ok`/`Err` | `desired == Waiting` | record `teardown_error`; state → `Pending`; `Settled` |
| `Unload` | `Ok`/`Err` | `desired == Ready(epoch)` | record `teardown_error`; `Start(Load(epoch))` |
| `Unload` | `Ok`/`Err` | `desired == Retired` | record `teardown_error`; state → `Disposed`; `Settled` |

Note that an `Unload` records an error but **never blocks retirement**: a failed teardown still
reaches `Disposed` when the fiber is retired, and the supervisor surfaces it as `teardown_error`
(test: `unload_failure_is_recorded_but_does_not_block_retirement`).

## Relation to fiber.md

This page is the deeper reference. The public API, the `FiberState`/`DesiredState`/`EpochEntry`
types, and the latency of `restart`/`reload` are in [fiber](fiber.md); the supervisor's consumption
of `TransitionAdvance` — dispatching executors, blocking unloads on consumers, and cycle detection —
is in [supervisor](supervisor.md).
