# Cordis 0.1.0 release checklist

## Required gates

- [ ] Public API review completed with semver decisions recorded.
- [ ] Core lifecycle/property model tests cover generated operation sequences.
- [ ] Fault injection covers activation, teardown, routing, watcher, and rollback failures.
- [ ] Miri passes for supported Core/effect/collection targets.
- [ ] Loom models the synchronization primitives that remain custom after API review.
- [ ] Linux, macOS, and Windows CI is green on the release commit.
- [ ] MSRV 1.98.0 check, strict Clippy, rustdoc warnings, and formatting are green.
- [ ] `wasm32-wasip2` provider/consumer runtime composition E2E is green.
- [ ] Dependency license/advisory review is recorded.
- [ ] Benchmarks identify a measured bottleneck before optimization changes are accepted.
- [ ] README, semantic differences, CLI help, examples, and changelog are current.
- [ ] Crate package contents and dry-run publication have been inspected.

The workspace version being `0.1.0` is not release evidence. None of these unchecked gates may
be inferred from a local unit-test pass.
