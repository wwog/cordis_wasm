# Cordis 0.1.0 release checklist

## Required gates

- [x] Public API review completed with semver decisions recorded.
- [x] Core lifecycle/property model tests cover generated operation sequences.
- [x] Fault injection covers activation, teardown, routing, watcher, and rollback failures.
- [x] Miri passes locally for the supported Core effect and tracked-collection targets; the same
  commands run in the nightly CI job.
- [x] Loom applicability reviewed: no custom synchronization primitive remains to model.
- [ ] Linux, macOS, and Windows CI is green on the release commit.
- [x] MSRV 1.98.0 check, strict Clippy, rustdoc warnings, and formatting are green locally.
- [x] `wasm32-wasip2` provider/consumer runtime composition E2E is green locally.
- [x] Dependency license/advisory review is recorded and green; one informational unmaintained
  transitive dependency is documented in `dependency-review.md`.
- [x] Benchmarks establish a baseline before optimization-specific public API is accepted.
- [x] README, semantic differences, CLI help, examples, and changelog are current.
- [x] Package contents inspected; core/macros/guest tarballs verify independently, and the complete
  graph compiles from extracted `.crate` archives with only packaged internal dependencies. The
  extracted CLI archive installs in release mode and reports `cordis 0.1.0`.
- [ ] crates.io ownership is confirmed for the occupied `cordis*` package names, or all packages
  and internal dependency versions are moved to an available naming scheme.

The workspace version being `0.1.0` is not release evidence. The remaining unchecked boxes require
a package namespace decision and the exact release commit to exist on the remote CI provider.
