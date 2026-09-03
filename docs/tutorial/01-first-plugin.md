# 1. Your first run

Before you write anything, run the shipped example end to end. The goal of this chapter is to see
the whole loop — build the guests, then `check` them, `inspect` them, and `run` them — and to read
the declarative configuration that ties them together. Once the loop is familiar, writing a new
plugin is mostly a matter of filling in the same shape.

Work from the repository root. The two guest crates are `examples/wasm-counter-provider` and
`examples/wasm-counter-consumer`.

## Build the guests

```sh
cargo run -p xtask -- build-guests
```

This invokes the `xtask` binary with the `build-guests` subcommand. It does three things, in order:

1. Checks that `wasm32-wasip2` is installed (`rustup target list --installed`); if not, it prints
   `missing Rust target "wasm32-wasip2"; install it with rustup target add wasm32-wasip2` and stops.
2. Runs `cargo build --target wasm32-wasip2 -p wasm-counter-provider -p wasm-counter-consumer`.
   The output lands at `target/wasm32-wasip2/debug/wasm_counter_provider.wasm` and
   `target/wasm32-wasip2/debug/wasm_counter_consumer.wasm` — the snake_case names are the crate names
   with hyphens replaced by underscores.
3. Runs the host integration tests with `CORDIS_GUEST_FIXTURES` pointing at that directory. The two
   tests that matter here — `guest_sdk_artifacts_run_end_to_end` and
   `declarative_guest_artifacts_check_mount_route_and_shutdown` — load the compiled components into a
   real engine, activate them, route a call, and shut down cleanly. If they pass, the artifacts are
   good enough to run.

Near the end you should see:

```
running 2 tests
test runtime::tests::guest_sdk_artifacts_run_end_to_end ... ok
test loader::tests::declarative_guest_artifacts_check_mount_route_and_shutdown ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out
```

The `.wasm` files in `target/wasm32-wasip2/debug/` are what the config refts to by path. Nothing in
the config loads a crate — it loads an *artifact*.

## The declarative config

Open `examples/wasm-app/cordis.json`:

```json
{
  "entries": [
    {
      "id": "consumer",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_consumer.wasm",
      "config": {},
      "isolate": {
        "example.counter": "example"
      }
    },
    {
      "id": "provider",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_provider.wasm",
      "config": {},
      "isolate": {
        "example.counter": "example"
      }
    }
  ]
}
```

Four fields matter:

- **`entries`** is a list, the application's plugin tree. Each object is one plugin to mount. (A
  tree — not just a list — because an entry may set `group: true` and carry `children`; the counter
  example keeps it flat.)
- **`id`** is the entry's stable identifier in the tree. Reconciliation is keyed on it: edit an
  entry's `config` while `run` is watching, and the loader updates *that* entry instead of
  restarting everything.
- **`component`** is a reference. The two forms are `builtin:<name>` and `file:<path>`. A `file:`
  path is resolved against the directory containing the config (so `../../target/...` is relative to
  `examples/wasm-app/`). A `builtin:` name must have been registered with the `BuiltinRegistry` —
  none of the shipped examples use it, but it is how an embedding host mounts process-local
  components without a `.wasm` file.
- **`config`** is a JSON object passed to the component's `activate` function when it starts. Here it
  is empty (`{}`), which is exactly what both guests' schemas allow — see chapter 5 for the schema
  that constrains it.

The **`isolate`** block is the part that does not look like a plain config declaration, and it is
central. Read it as `"service-name": "realm"`. Here `"example.counter": "example"` says: *the
service `example.counter` is bound to a realm labeled `example` within this entry.* Both the
provider and the consumer declare the same mapping, which is how they end up in the same realm and
therefore satisfy each other.

A `"realm"` value can be either a bare string (a **global** realm shared by every entry that names
the same label) or `true` (a **local** realm scoped to that entry alone and its children). The
`isolate` map on an entry says "for service `S`, route lookups to realm `R`," overriding whatever
the parent would otherwise supply. This is what lets two providers of the same service coexist: put
them in different realms and each consumer resolves whichever realm its context names.

The consumer also declares `"example.counter"` via `isolate` even though it **consumes** that
service rather than providing it. That is intentional and matters. The `isolate` overlay applies to
any service the entry's descriptor *touches* — the loader collects the union of the descriptor's
`inject`s and `provide`s and isolates each against the entry's map (see
`WasmEntryDriver::entry_context` in `crates/cordis-wasm/src/loader.rs`). So the consumer needs the
realm mapping to *find* the provider, and the provider needs it because a provider registers its
service in a realm and only consumers in that realm resolve it.

## Check — validate without activating

```sh
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
```

Expected output:

```
ok: 2 entries, 2 components
  example.wasm-counter-consumer
  example.wasm-counter-provider
```

`check` does **not** activate anything. It runs a preflight driver over the entry tree. For each
entry it resolves the `file:` path, reads the bytes, and runs `WasmComponentFactory::from_bytes` —
which compiles the component, queries its descriptor, and validates three things:

- the descriptor's `wit_version` matches the host `kernel_abi` (`"0.1"`), or you get
  `KernelAbiMismatch`;
- every capability in `capabilities` is in the policy's `allowed_capabilities` (default: none), or
  you get `CapabilityDenied`;
- any WASI import the component needs is both declared in `capabilities` and allowed by the policy —
  see `capability_for_wasi_import` in `crates/cordis-wasm/src/runtime.rs`, and chapter 6 for the
  network case.

It also validates each entry's `config` against the component's `config_schema`. The descriptor
`name` is what you see printed. The count `2 entries` is the number of entries; `2 components` is
the number of distinct descriptor names.

Because nothing is activated, `check` is cheap and is the right tool for "did I get the path or the
schema wrong." The counter component implements `Guest::call_service` and the consumer implements
`handle_event`, but neither runs during `check`.

## Inspect — actually mount everything

```sh
cargo run -p cordis-cli -- inspect examples/wasm-app/cordis.json
```

Expected output (fiber ids are integers that may differ):

```
fibers: 3
  fiber=1 parent=None state=Pending dependencies=0
  fiber=2 parent=Some(FiberId(1)) state=Active dependencies=1
  fiber=3 parent=Some(FiberId(1)) state=Active dependencies=0
```

`inspect` goes further than `check`: it constructs a real `WasmApplication`, reconciles the entries,
and calls `settle` — which waits for the runtime to reach quiescence — then prints the fiber tree.
Here the difference from `check` shows up immediately:

- `fiber=1` is the **root** fiber, created by the application and never given a component. It is
  `Pending` because a root fiber has no component to activate.
- `fiber=2` and `fiber=3` are the two entries. Both are `Active`, meaning their `activate` ran and
  completed without error.

The `dependencies` column is telling. `fiber=2` has `dependencies=1` — that is the **consumer**,
which injects `example.counter`. `fiber=3` has `dependencies=0` — that is the **provider**, which
only provides. `parent=Some(FiberId(1))` says both are children of the root.

This is the "punchline" of the reactive model in microcosm: the consumer only reached `Active`
because the provider moved first. The provider declares no dependencies, so it activates
immediately; the consumer declares a required inject for `example.counter`, so its fiber stays
`Pending` until a provider is registered in the realm it resolves. When that happens, the supervisor
notifies the consumer, which then activates. `inspect` shows you the *settled* state where the
ordering has already happened.

## Run — the live loop

```sh
cargo run -p cordis-cli -- run examples/wasm-app/cordis.json
```

The output and then a wait until you press Ctrl-C:

```
running 2 fibers across 2 artifacts; press Ctrl-C to stop
```

```
stopped 3 fibers
```

`run` does everything `inspect` does, then stays up. Specifically:

1. It builds the application, reconciles the entries, and waits for quiescence — so the two guest
   fibers become `Active` here too.
2. It registers the `ConsoleExporter` with the application's logger (`cordis-cli/src/main.rs`
   calls `application.driver().logger().register_exporter(...)`).
3. It prints `running 2 fibers across 2 artifacts; press Ctrl-C to stop`. The `2` is
   `snapshot.fibers.len() - 1` — the number of *non-root* fibers. The `2` artifacts is the count of
   distinct artifact paths currently tracked for hot reload. Both guests are `file:` components, so
   both count.
4. It sets up an `HmrWatcher` on those artifact paths **plus** the config path, and enters a
   `tokio::select!` loop waiting on Ctrl-C and on filesystem change events.

When you press Ctrl-C, `run` shuts down the application (reconciling to an empty tree stops the
entries child-first, then retires the root fiber) and prints `stopped 3 fibers` — the root plus the
two guests.

Try it once, let it sit, and then edit `examples/wasm-app/cordis.json` in another terminal while it
runs: you should see `config: committed 2 active fibers` as the tree reconciles, or a rollback
message if you make the config invalid. That is the config watch path working. Rebuilding a guest
`.wasm` file and touching it triggers the artifact watch: `hmr: committed N entries`.

## What you have now

You have seen the four commands that anchor everything else in this tutorial. The key mental model
to take into the next chapter:

- A guest is a **component artifact** (`.wasm`), not a crate that gets linked.
- The **config** is a declarative tree of entries; each entry names a component reference, a config,
  and a set of realm isolations.
- A component's **descriptor** is read *before* activation and decides what it needs to run and what
  it offers.
- `check` validates, `inspect` mounts and settles, `run` mounts, settles, and watches.

Next: [Anatomy of a guest plugin](02-plugin-anatomy.md) — the `Guest` trait, `PluginDescriptor`, and
how `activate` wires a component into the kernel.
