# 0.1.0 benchmark baseline

The release policy does not accept optimization-specific public knobs without a measured
bottleneck. The baseline is a dependency-free, stable-Rust benchmark executable:

```bash
cargo bench -p cordis-core --bench lifecycle
```

Reference run on 2026-09-03, Apple Silicon macOS, Rust 1.98.0, optimized bench profile:

| Scenario | Iterations | Baseline |
|---|---:|---:|
| Resolve a realm through 32 immutable Context overlays | 1,000,000 | 64 ns/op |
| Complete one Fiber load/unload round trip | 250,000 | 17 ns/op |

These numbers are observations, not regression thresholds: shared CI runners are too noisy for
hard nanosecond gates. No 0.1.0 public tuning knobs were added from this baseline. A later change
must reproduce a bottleneck under the same scenario, add a representative benchmark when needed,
and report both absolute cost and relative improvement before changing public API for speed.
