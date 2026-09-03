# Dependency policy review for 0.1.0

Review date: 2026-09-03.

Commands executed against the release-candidate `Cargo.lock`:

```bash
cargo-deny 0.20.2 check bans licenses sources advisories --hide-inclusion-graph
cargo-audit 0.22.2 audit
```

Results:

- cargo-deny: advisories, bans, licenses, and sources all passed. Duplicate transitive versions
  are warnings because Wasmtime 48 and wit-bindgen 0.61 intentionally resolve different versions
  of several WebAssembly and platform support crates.
- cargo-audit: no vulnerability caused failure. It reported the informational unmaintained
  advisory `RUSTSEC-2026-0249` for `smartstring 1.0.1`.
- `smartstring` is build-time/transitive through `rhai 1.26.0`, is not exposed by Cordis public
  API, and has no vulnerability advisory in this scan. Rhai is already constrained to the current
  1.26 line used by the restricted Include expression evaluator. The release accepts this
  informational warning and will remove or replace the dependency when Rhai provides a maintained
  path; a vulnerability advisory remains release-blocking.
- Every publishable path dependency now carries `version = "0.1.0"`; example path dependencies do
  as well, so the wildcard-dependency policy passes.
- Allowed source is crates.io only; unknown registries and git sources are denied.

CI repeats cargo-deny and RustSec checks on every push and pull request. A new advisory can therefore
block a later release even when the source tree is unchanged.
