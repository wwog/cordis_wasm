# 8. Troubleshooting

The runtime is explicit about failure — it would rather fail loud at load time than silently defer
to a confusing runtime error. This chapter lists the failure modes you are most likely to hit, what
each one actually means, and how to read it. Most are checked by `cordis check` before anything
activates, and a few only surface in `run`.

## PENDING / never-activating: a missing provider for an `inject`

The most common case is a fiber that stays `Pending` and seems to do nothing.

**Symptom.** `cordis inspect` shows a `state=Pending` fiber that never becomes `Active`. There is no
error message at all — the fiber simply does not advance.

**Cause.** A required `inject` has no provider in the realm the consumer resolves. The descriptor can
declare all the right services, but the *realm wiring* is wrong. Recall chapter 3: routing is a
function of the caller's context, and the `isolate` map decides which realm a service resolves in.
The consumer stays `Pending` until a provider registers in that realm.

**The most common wrong reason.** The consumer and provider declare *different* `isolate` mappings
for the same service — say the consumer has `"example.counter": "example"` but the provider has
nothing, or has a different label. They never meet. Check both entries in `cordis.json` name the
same realm label for the same service name. This is why the counter example repeats the mapping on
*both* entries.

**Other causes.** The provider never called `provide_service` in `activate` (so it is a candidate but
no provider is present). Or the provider's fiber `Failed` right after loading — in which case the
consumer sees no provider and stays `Pending`. Look at the provider's own state before blaming the
consumer.

**Check it.** Compare the `isolate` blocks and confirm the provider is `Active`. A required inject
that never resolves is by design a silent `Pending` — the runtime is waiting, not failing. The paper
calls this the "dependency cycle leaves components permanently inactive... predictable from the
dependency declarations alone" case; see [semantics.md](../semantics.md).

## `KernelAbiMismatch`

**Symptom.** A `Descriptor` or `Driver` error containing `kernel ABI mismatch: expected 0.1, got X`.

**Cause.** The guest's `PluginDescriptor::wit_version` does not equal the host's
`ArtifactPolicy::kernel_abi`. The host requires `"0.1"` (from `ArtifactPolicy::default`), and the
guest must set `wit_version: cordis_guest::KERNEL_ABI` — which is `"0.1"`. If you copied an old
descriptor or hard-coded a different string, the mismatch appears at load time.

**Fix.** Set `wit_version: cordis_guest::KERNEL_ABI.into()`. The check is in
`validate_descriptor` (runtime.rs):

```rust
if descriptor.kernel_abi.as_ref() != policy.kernel_abi {
    return Err(WasmHostError::KernelAbiMismatch { expected: ..., actual: ... });
}
```

This is the safety valve for the "two WIT copies must match" rule. If the kernel WIT ever changes,
both the guest SDK and the host bump `KERNEL_ABI` together, and an old artifact is rejected rather
than silently mis-communicated.

## `CapabilityDenied`

**Symptom.** `cordis check` (or `run`) fails with `component capability \`network\` is denied`, or
`WASI import ... requires undeclared capability network`.

**Cause.** Two separate checks, both in `validate_descriptor` / `validate_wasi_imports`:

- The **declared** capability is not in `ArtifactPolicy::allowed_capabilities`. Your descriptor lists
  `"network"` but the policy (the CLI's default, which allows nothing) does not.
- The **imported** WASI interface needs a capability the descriptor did not declare. A guest that
  imports `wasi:sockets/` or `wasi:http/` but omits `"network"` from its `capabilities` list gets
  this error from `validate_wasi_imports` even if the policy *would* allow `network`.

The first is "you asked for permission the host won't grant." The second is "you used a thing you
didn't declare." Both are loud, at load time, before activation — which is the point. See chapter 6
for the full account of what a network-capable guest needs. The fix requires a host that constructs
an `ArtifactPolicy` granting the capability; the CLI does not expose that today.

## `InvalidConfig` / schema errors

**Symptom.** `entry <id> configuration is invalid at <path>: <message>`, or a `Driver` error from
`check` mentioning a schema.

**Cause.** The entry's `config` does not match the component's `config_schema`. The exact path in the
error tells you which field. Chapter 5's `additionalProperties: false` guardrail is the usual trigger
— an unexpected key, or the wrong type for a declared property.

**Fix.** Either correct the config to match the schema, or, if the plugin genuinely needs the field,
update the `config_schema` to declare it. Note the schema is *the component's* declaration: a plugin
that wants `port` as an integer must declare it, or the host rejects an integer config as a schema
violation.

**A related trap.** A config that is valid JSON but not a valid JSON Schema in the *descriptor*
(`config_schema`) is caught separately as a `Descriptor` error — the host parses it with
`serde_json` and then `Schema::try_from`. So a malformed `config_schema` (not just a malformed
`config`) is also a load-time failure. The bytes must be valid JSON *and* a valid Draft 2020-12
schema.

## `wasm32-wasip2` target missing

**Symptom.** `cargo build --target wasm32-wasip2 -p ...` fails with an error about an unknown target,
or `xtask build-guests` prints
`missing Rust target "wasm32-wasip2"; install it with rustup target add wasm32-wasip2`.

**Cause.** The guest components compile to WASIp2, not to the older `wasm32-wasi`. If the target is
not installed, nothing builds.

**Fix.**

```sh
rustup target add wasm32-wasip2
```

Then verify with `rustup target list --installed` — the line `wasm32-wasip2` should be present.

## `cordis check` vs `cordis run` — the difference

The two commands share most of their path but differ in one vital way, and that difference is a
common source of confusion:

| | `check` | `run` (and `inspect`) |
|---|---|---|
| Reads config and resolves entries | Yes | Yes |
| Compiles every component, reads descriptors | Yes | Yes |
| Validates config against schema | Yes | Yes |
| Checks ABI and capability policy | Yes | Yes |
| **Activates components** | **No** | **Yes** |
| Registers a log exporter | No | Yes |
| Watches for HMR | No | Yes |

The practical consequence: `check` can succeed while `run` fails, because `check` never runs
`activate`. A guest that activates correctly at `check` but fails in `activate` (a decode error, a
`provide_service` that returns an error, a panic) will pass `check` and fail `run`. Conversely,
`check` is the fast "did I wire the declaration right" tool that avoids the cost of mounting
everything.

The other way the two diverge: `check` uses a `PreflightDriver` that stops without doing anything at
`stop`, whereas `run`/`inspect` mount real `DynamicFiber`s. So a fiber-level lifecycle error that
would only appear after activation is invisible to `check`.

Diagnose in this order: run `check` (declaration + policy + schema), then `inspect` (does it
actually mount and settle? what state are the fibers in?), then `run` (activate + log + watch).

## HMR watching config vs artifact

`cordis run` watches **both** the config file and every `file:` artifact the app is currently
running. The two watch paths behave differently:

- **Config change** → `application.reconcile(entries)`. The config is reloaded and diffed; a valid
  change commits (`config: committed N active fibers`), an invalid one rolls back entirely
  (`config: transaction rolled back: <error>`). The whole previous tree is preserved on failure.
- **Artifact change** → `application.driver().reload_paths(paths)`. The touched `.wasm` is recompiled
  and the affected fibers replaced transactionally: `hmr: committed N entries` on success,
  `hmr: transaction rolled back: [<entries>]` on failure.

**Symptom of "nothing happens when I edit".** You edited a config value, rebuilt a guest, or touched
a `.wasm`, and the running app did not respond. Check which file actually changed on disk and that
it is in the watched set. The watcher tracks the canonical artifact paths (`artifact_paths()`) plus
the config path. If you rebuilt into a *different* path than the one the config points at, or edited
a source file that is not recompiled to the artifact path, nothing changes — the watcher keys on the
artifact bytes, not the source. Rebuild the artifact (`cargo build --target wasm32-wasip2 -p ...`)
and make sure the config's `file:` path points at the rebuilt output.

Also note: `check` has no watch path at all. Only `run` watches.

## A fiber `Failed` during activation

**Symptom.** `inspect` shows a fiber in `Failed` state, or `run`/`inspect` reports a component error
at start. The message often is a `ComponentFailed` wrapping the guest's error.

**Cause.** `activate` returned an error, or a panic inside the guest was caught. Chapter 2's "make
`activate` throw" is the loud version. Common guest-side causes: `decode` of the config fails (the
config bytes do not match what the guest expects), `provide_service` / `register_listener` returns a
kernel error (for example `CapabilityDenied` for a registration limit, or a duplicate listener id).

**Fix.** Make `activate` return a meaningful `KernelError` instead of panicking. The runtime wraps a
guest panic as `component panic while polling guest call` and marks the fiber `Failed`; a returned
error is surfaced as the kernel error. Prefer returning an error over a panic so the message is the
actual cause.

## The config `file:` path is wrong

**Symptom.** `InvalidComponentRef` or a "no such file" `Driver` error when `check` or `run` resolves
the entry.

**Cause.** The `component` value is not a valid `file:` or `builtin:` reference, or the path does not
resolve against the config's directory. `ComponentRef::parse` requires the `file:` prefix and a
non-empty path; the path is joined to the config's parent directory.

**Fix.** Use `file:` (for a path relative to the config) or `builtin:<name>` (for a registered
builtin). Confirm the path resolves relative to the file containing the config, not to the CWD. The
unknown-scheme error is a typo guard: a bare path with no `file:` prefix is rejected.

## Where to look next

- The API reference covers the exact signatures behind each of these: [guest](../api/index.md) for the
  `Guest` trait, [service](../api/index.md) for `ServiceId`/`InjectSpec`, [event](../api/index.md) for
  `EventMode`/`EventReply`, [wasm](../api/index.md) for `ArtifactPolicy`/`WasmLimits`.
- The semantic model behind each behavior is in [semantics.md](../semantics.md) — in particular the
  "component activates only when its dependencies are provided" rule (Theorem 70) that explains the
  PENDING case, and the transactional reconcile / HMR tables.
- The full error enums: `LoaderError` in `crates/cordis-loader/src/entry.rs`, `WasmHostError` in
  `crates/cordis-wasm/src/lib.rs`, and `CordisError` in `crates/cordis-core/src/error.rs`.

This is the last chapter. You now have the tools to read a `cordis.json`, write a guest that provides
a service, listen to an event, and diagnose why a plugin will not start. The honest boundary from
chapters 6 and 7 — no `wasi:http`, no UI — is not a gap in your understanding but the current edge of
the runtime. When you extend it, start with the host `ArtifactPolicy` and the guest `capabilities`
list, and the rest will follow.
