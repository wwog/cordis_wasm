# Cordis (Rust) Tutorial

Cordis is a Rust plugin runtime built on a **revertible-effect** model: every effect a plugin
performs is paired with an inverse the runtime applies, LIFO, when that plugin unloads. It layers
**reactive dependency injection** on top — a plugin activates only once every service it declares is
actually provided. Dynamic plugins run inside **Wasmtime's Component Model**, so the guest code is
a WebAssembly component that talks to the host through a versioned WIT kernel interface rather than
through the Rust ABI.

This tutorial is about writing those dynamic plugins: a WebAssembly component that *provides* a
service, *consumes* another, *reacts* to an event, and is composed into an application by a
declarative `cordis.json` file. By the end you will have written two real plugins — one that exposes
an HTTP-style web service, and one that listens for an event and surfaces the result — and you will
know exactly where the runtime's real support ends and where it is still skeletal.

The reader is an agent or plugin developer who wants to understand this repository's plugin model.
You do not need prior WebAssembly Component Model experience, but you should be comfortable reading
Rust and following the host/guest boundary. The TypeScript Cordis sibling project
([`ts-docs/cordis-tutorial`](../ts-docs/cordis-tutorial/index.md)) is the conceptual ancestor of this
runtime; this tutorial mirrors its structure where the semantics line up and diverges where they do
not.

## What this tutorial is not

This is not a Rust language guide and not a Wasmtime tutorial. It assumes you can build a project
with Cargo. It *is* honest about the runtime: where a feature is skeletal — HTTP server support and
any notion of a "view" are the two big ones — the chapters say so rather than inventing an API.
Code that is a sketch rather than a working crate is marked **illustrative**, and each such block
states which parts are real and which you must supply.

## Prerequisites

You need a working Rust toolchain with the WebAssembly Component Model target installed.

- **Rust 1.98** (the workspace pins `rust-version = "1.98"` and `rust-toolchain.toml` selects
  `channel = "1.98.0"` with `clippy` and `rustfmt`).
- The **`wasm32-wasip2`** target. The guest components are compiled to WASIp2, not to the older
  `wasm32-wasi`:

  ```sh
  rustup target add wasm32-wasip2
  ```

You can confirm it is installed with `rustup target list --installed`; the line should include
`wasm32-wasip2`.

- **`cargo`**, and — for the artifacts this tutorial builds — `wasm32-wasip2`'s `cargo build`
  support, which ships with the toolchain above. No network access is needed for any command in
  this tutorial; every plugin here is a local component loaded by path.

You do **not** need to install `cordis` globally. You run it through Cargo:
`cargo run -p cordis-cli -- <subcommand> <config>`.

## The directory you will work in

This repository is a Cargo workspace. The pieces you care about live in a small number of places:

| Path | What it is |
|---|---|
| `crates/cordis-guest/` | The guest SDK: generated kernel WIT bindings, the `Guest` trait, and the helper exports you implement. |
| `crates/cordis-guest/wit/kernel.wit` | The WIT kernel world a guest component implements. The host copy lives at `crates/cordis-wasm/wit/kernel.wit`; a test asserts the two are byte-identical. |
| `crates/cordis-wasm/` | The host side: the Wasmtime engine, limits, factory, capability policy, loader driver, and the kernel host that routes calls to active fibers. |
| `crates/cordis-cli/` | The `cordis` command: `check`, `inspect`, `run`, `build-component`. |
| `examples/wasm-counter-provider/` | A complete guest that *provides* a counter service. Your template for chapter 6. |
| `examples/wasm-counter-consumer/` | A complete guest that *injects* that same counter service and calls it during `activate`. Your template for chapter 7. |
| `examples/wasm-app/cordis.json` | The declarative application that composes the two examples. |
| `docs/api/index.md` | The API reference index. The per-crate pages it lists are where each chapter points for the deep dive. |

You will not create new top-level crates in this tutorial's core chapters — you will read
`examples/` first and then write your own guests alongside them. Because the workspace builds the
guest examples as part of `xtask`, keeping your own guest crates in `examples/` (or as sibling
workspace members) is the path of least resistance.

## How to run the reference example

The two shipped guests, `wasm-counter-provider` and `wasm-counter-consumer`, are the end-to-end
truth check for everything this tutorial teaches. Run them once, before you write anything, so the
load loop is familiar:

```sh
cargo run -p xtask -- build-guests
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
cargo run -p cordis-cli -- inspect examples/wasm-app/cordis.json
cargo run -p cordis-cli -- run examples/wasm-app/cordis.json
```

The first command builds both guest components to `target/wasm32-wasip2/debug/` and then runs the
host-side integration tests against them. `check` validates the declaration and every component
without *activating* any of them. `inspect` goes one step further and mounts every entry, then
reports the resulting fiber tree. `run` does all of that, registers a console log exporter, and then
watches the config and the artifacts for hot changes until you press Ctrl-C.

Chapter 1 walks through each of these commands line by line.

## Chapters

1. [Your first run](01-first-plugin.md) — run the shipped counter example and read its `cordis.json`.
2. [Anatomy of a guest plugin](02-plugin-anatomy.md) — the `Guest` trait, `PluginDescriptor`, `export_plugin!`, and `CallContext`.
3. [Services and injection](03-services-and-inject.md) — `provide`/`inject`, `ServiceId` and the ABI hash, realms, and the native macro contrast.
4. [Events](04-events.md) — the WIT event surface, the five `EventMode`s, `EventReply`, and listener registration.
5. [Configuration and the sandbox](05-config-and-capabilities.md) — config bytes, the JSON Schema, `ArtifactPolicy`, `WasiCapabilities`, `WasmLimits`.
6. [Writing a web server plugin](06-writing-a-web-server-plugin.md) — the main exercise: a plugin that provides an HTTP service, and the real network-capability boundary.
7. [Events and views](07-events-and-views.md) — the second main exercise: a plugin that reacts to an event and surfaces the result where one can see it.
8. [Troubleshooting](08-troubleshooting.md) — common failure modes and how to read them.

## Where the API reference lives

The full API reference is at [the API index](../api/index.md). Each crate has its own page under
`../api/` (`guest.md`, `service.md`, `event.md`, `wasm.md`, `loader.md`, and so on); the index lists
every page and the exact public items it documents. Chapters link to the specific page when you want
a signature, an `# Errors` section, or the deeper model behind what the walkthrough glosses over.

The semantic foundation — how this implementation maps onto the paper *A Programming Paradigm for
Spatiotemporal Composability* — is [semantics.md](../semantics.md). It is the authoritative source
for *why* a behavior exists; the tutorial is the source for *how to use it*.

Next: [Your first run](01-first-plugin.md) — run the counter example and read its `cordis.json`.
