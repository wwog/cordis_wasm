# 7. Events and views

The second main exercise: a plugin that, when some event fires, reacts and surfaces the result in a
place you can observe. This is where the word "view" needs its honest definition, because the repo
has no DOM, no UI framework, no screen.

## What "view" means here

There is no graphics layer in `cordis-wasm` and no notion of rendering a plugin's output to a user.
A "view" in this runtime is an **observable surface you write to** when an event fires. You have
four realistic places, in order of how directly they work today:

1. **The log, via `host::log`.** The host import `log(context, level, message)` writes to the
   application's `Logger`. In `cordis run`, the `ConsoleExporter` is registered, so a guest that
   logs emits a line to stderr:
   ```
   [Info] [cordis.guest] [fiber=<id>] status: ready
   ```
   This is the most direct "surface" — you write a line and it appears. A guest calls
   `host::log(context, "info", &message)` (case-insensitive level; unknown levels map to `Info`).

2. **A service the host asks.** A plugin can `provide` a service whose `call_service` returns the
   latest observed value on demand. A host-side component — or another guest — calls it to read the
   status. This is "a view" in the sense of a *readable endpoint*: something external pulls.

3. **An `EventReply`.** When a listener reacts, it returns `ContinueValue` or `BreakValue`. The
   emitter (the provider that dispatched the event) receives that reply and can act on it — so the
   "surface" is the return value of `dispatch_event`. This is how a plugin hands a decision or data
   back to whoever triggered the event.

4. **`cordis run`'s human-readable snapshot.** Not a per-plugin thing, but the CLI's `inspect`
   output (`state=Active`, `dependencies=N`) is a live, textual view of the fiber tree. It is slow
   and static, but it is genuinely where an operator "sees" the app.

So "display in a view" translates concretely to: **declare or reuse an event, register a listener,
react in `handle_event`, and write the result to one of the surfaces above.** The most useful and
simplest is the log; a status/telemetry surface is a service that holds the latest value and lets a
caller read it.

This chapter builds a status/telemetry plugin: a provider emits a `status` event with a message, and
a consumer listens and logs it. We use a service to hold the latest value so it can also be pulled.

## Declare (or reuse) an event

There is no event-declaration macro in the guest SDK, so an event is a `EventId` you construct by
hand, exactly like a service:

```rust
const STATUS_ABI: [u8; 32] = [0x51; 32];   // convention: agree across emitter and listener

fn status_event() -> EventId {
    EventId {
        name: "example.status".into(),
        abi_hash: STATUS_ABI.to_vec(),
    }
}
```

The emitter and listener must agree on name *and* hash. The hash pins the payload contract — here
the payload is a `String` (the message). If you ever change the payload type, change the hash too;
that is the whole point of the ABI hash.

Where does the event come from? Two options:

- **Reuse an existing one** — the counter provider already has a `handle_event` that echoes
  `ContinueValue(payload)`. You can register a listener for any `EventId` you and the emitter agree
  on. But note: an event is only meaningful if something *dispatches* it. The provider gives you the
  machinery; the *emission* is what you write.
- **Declare your own** — define a `status_event()` as above and emit it from a provider. That is the
  path this chapter takes: the emitter is your plugin, so you control both sides.

## Register a listener

A listener is registered in `activate`, via the host import:

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::register_listener(
        context,
        &status_event(),
        STATUS_LISTENER_ID,
        EventMode::Serial,
    )?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

`STATUS_LISTENER_ID` is a `u64` you choose — it is this listener's stable key in the host's map. The
`EventMode::Serial` says the listener is part of a serial (ordered, stop-at-Break) dispatch. As
chapter 4 noted, the host routes a single listener per dispatch today, so the mode is carried through
for the listener to interpret, not for the host to orchestrate a fan-out.

One rule matters: the `(event, listener_id)` pair must be unique per fiber. Register two different
listener ids to react to two different events.

## Emit from a provider

A provider emits by calling `dispatch_event`:

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let payload = cordis_guest::encode(&"status: ready".to_string())?;
    let reply = host::dispatch_event(
        context,
        &status_event(),
        STATUS_LISTENER_ID,
        EventMode::Serial,
        &payload,
        None,          // next_token — see below
    )?;
    // `reply` is the listener's EventReply; you could log it or act on it.
    Ok(())
}
```

The emitter names the `listener_id` it is addressing. This is the "emit to a specific listener"
model from chapter 4 — a consequence of the single-listener dispatch boundary, and one you will
feel here. If you register several listeners, emit to each by its id, or extend the host's
`dispatch_event` to fan out.

`next_token` is the waterfall one-shot token. The native `WaterfallEvent` in
`crates/cordis-core/src/event.rs` passes a `Next` continuation so a listener can wrap the downstream
result. In the guest path, `next_token` is carried through into `handle_event`, but the host kernel
does not manage the token chain — it is the guest's responsibility to interpret it. If you are not
building a waterfall, pass `None`.

## React and surface the result

The listener's `handle_event` turns the event into an observable result. The version that logs:

```rust
fn handle_event(
    context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

`host::log` is the surface. In `cordis run` it reaches stderr via `ConsoleExporter`. The reply is
`ContinueValue` because a logger has nothing decisive to say — it observed and moved on. If instead
the listener were a *policy* that could veto or answer, it would return `BreakValue(encoded)`, and
the emitter would treat that as the result.

### A status/telemetry service surface

Logging is "push": the value goes out when the event fires. A *pulled* view is a service the plugin
provides, whose `call_service` reads the latest value. Combine the two — the listener both logs and
updates a thread-local, and the service lets a caller read it.

```rust
thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static LATEST_STATUS: RefCell<String> = const { RefCell::new(String::new()) };
    static STATUS_SERVICE_REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
}

fn status_service() -> ServiceId {
    ServiceId { name: "example.status".into(), abi_hash: STATUS_ABI.to_vec() }
}

// In activate, also provide the service:
//   let reg = host::provide_service(context, &status_service())?;
//   STATUS_SERVICE_REGISTRATION.with(|slot| *slot.borrow_mut() = Some(reg));

fn call_service(
    _context: CallContext,
    service: ServiceId,
    method: u32,
    _payload: Vec<u8>,
) -> Result<Vec<u8>, KernelError> {
    if service.name != status_service().name || method != GET_METHOD {
        return Err(KernelError::InvalidArgument("unknown status method".into()));
    }
    let latest = LATEST_STATUS.with(|slot| slot.borrow().clone());
    cordis_guest::encode(&latest)
}

fn handle_event(
    context: CallContext,
    _event: EventId,
    _listener_id: u64,
    _mode: EventMode,
    payload: Vec<u8>,
    _next_token: Option<u64>,
) -> Result<EventReply, KernelError> {
    let message = cordis_guest::decode::<String>(&payload)?;
    LATEST_STATUS.with(|slot| *slot.borrow_mut() = message.clone());
    host::log(context, "info", &format!("status: {message}"))?;
    Ok(EventReply::ContinueValue(payload))
}
```

Now the surface is two-headed: an event **pushes** a line to the log, and a service **pulls** the
latest value. A host-side component, or another guest with `inject: [status_service()]`, can call
`example.status` to read the most recent status on demand. This is the concrete "status/telemetry
surface" the chapter promised, and it uses only real APIs.

## The concrete plan for an event-driven status surface

Put together, the plan is:

1. **Pick an event** — `example.status` with a message payload and a fixed ABI hash.
2. **Emitter** (provider): in `activate`, encode a `String` and `dispatch_event` to the listener id.
   To make it continuous rather than one-shot, do it from a background loop — the guest SDK trusts
   the host to own cleanup, so a task that emits periodically is a legitimate pattern once you have
   a way to run it. (The `GuestTaskGroup` on the host aborts and joins host tasks at teardown.)
3. **Listener** (consumer or the same plugin): `register_listener` in `activate`, and in
   `handle_event`, decode the payload, update a thread-local, and `host::log` it.
4. **Pull surface**: `provide` a `status` service whose `call_service` returns the thread-local.
5. **Compose** both in `cordis.json` with an `isolate` binding so they land in the same realm.

The parts that are fully real: the event id, `register_listener`, `dispatch_event`, `handle_event`,
`host::log`, `provide_service`/`call_service`, and the encode/decode boundary. The parts you supply:
the exact payload type and hash, the loop cadence, and the log level.

## Where the UI boundary ends

If "view" means a live visual, this is where the honest line is. The repo does **not** render. There
is no browser, no canvas, no IPC to a frontend. The closest you get to a "live view" is:

- stderr lines during `cordis run` (via `host::log` → `ConsoleExporter`),
- a `cordis inspect` snapshot (a static, settled fiber tree),
- a service endpoint another component pulls.

Building an actual HTML/UI view would require something outside this runtime — an embedding host that
turns `host::log` or the `status` service into a rendered surface, or a frontend that calls the
service over another channel. That is a deliberate boundary, not an oversight: the runtime is a
plugin host, not a UI toolkit. If you want a real view, the plugin should *expose the data* (as a
service or an event) and let the embedding layer present it.

That is also the correct separation of concerns. A plugin that owns "here is the current status" and
a plugin that owns "draw it" are two different concerns; the runtime gives you the first, and the
second is the embedding layer's job. The `host::log` / `status` service approach is the intended
mechanism for the first.

## Try breaking it

- Register the listener with a `listener_id` that a provider then never emits to. Nothing visible
  happens — the event never fires at that id. This is the event analog of a missing provider: the
  listener is armed but never triggered, and the only clue is its absence.
- Emit to a `listener_id` no one registered. The host returns `listener {id} is not registered`.
  Check the id constant on both sides — they must agree, like the service hash.
- Decode the payload as the wrong type (for example `u64` instead of `String`). `handle_event`
  returns `InvalidArgument` from `decode`, and the listener fails. The ABI hash is meant to prevent
  this mismatch from being a silent surprise.

Next: [Troubleshooting](08-troubleshooting.md) — common failure modes and how to read them.
