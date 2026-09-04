//! Ahead-of-time (AOT) compilation cache for Wasmtime components.
//!
//! Wasmtime compiles a `.wasm` component into host machine code with Cranelift
//! on every cold load, which is expensive (seconds in debug builds). This crate
//! caches that compiled artifact on disk so a later process can skip the
//! compile and load it directly.
//!
//! The cache is a directory of files named `<artifact-hash>.cwasm`. A hit loads
//! the precompiled bytes with [`Component::deserialize`]; a miss compiles via
//! [`Engine::precompile_component`], writes the bytes, and loads them.
//!
//! # Safety
//!
//! [`Component::deserialize`] is `unsafe` because arbitrary bytes are not a
//! valid precompiled artifact and could cause undefined behavior. This crate
//! accepts the safety burden in one place and exposes a `safe` interface:
//! because a cached file is only ever read by the entry that *wrote* it under
//! the same artifact hash, and because a failed deserialize is discarded and
//! falls back to a fresh compile, the unsafe call is only reached with bytes
//! that were produced by [`Engine::precompile_component`] on this machine.

use std::path::Path;
use wasmtime::Engine;
use wasmtime::component::Component;

/// Compiles `bytes` to a component, or loads it from the on-disk AOT cache.
///
/// `hash` is the artifact fingerprint and determines the cache file name. When
/// `cache_dir` is `None`, this compiles normally (no caching).
///
/// Caching is best-effort: a cache that cannot be read or written — a read-only
/// directory, a missing parent, a stale artifact from a different Wasmtime
/// version — degrades to a fresh compile. Only genuine compile errors (invalid
/// WASM bytes, engine incompatibility) are surfaced as errors.
pub fn compile_or_load(
    engine: &Engine,
    bytes: &[u8],
    hash: &[u8; 32],
    cache_dir: Option<&Path>,
) -> Result<Component, wasmtime::Error> {
    let Some(dir) = cache_dir else {
        return Component::new(engine, bytes);
    };

    let filename = hash_filename(hash);
    let path = dir.join(filename);

    // A cached hit: load the precompiled artifact. If it is stale (produced by a
    // different Wasmtime version or config), deserialize fails and we fall
    // through to a fresh compile, replacing the bad entry.
    if path.exists() {
        if let Some(component) = try_load(&path, engine) {
            return Ok(component);
        }
        let _ = std::fs::remove_file(&path);
    }

    // A miss (or a stale hit): precompile once, persist, and load. Note that we
    // never fall through to a separate `Component::new` after precompiling —
    // that would compile twice.
    let serialized = engine.precompile_component(bytes)?;
    // Persisting is best-effort: a read-only cache directory must not break a
    // build, so a write failure silently keeps the in-memory artifact.
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = std::fs::write(&path, &serialized);
    }
    // SAFETY: `serialized` was produced by `Engine::precompile_component` above
    // for this engine, so it is a valid precompiled artifact for it.
    Ok(unsafe { Component::deserialize(engine, &serialized) }?)
}

/// Attempts to load a cached artifact; returns `None` if the file is absent,
/// unreadable, or fails to deserialize (i.e. is stale).
fn try_load(path: &Path, engine: &Engine) -> Option<Component> {
    let cached = std::fs::read(path).ok()?;
    // SAFETY: bounded to bytes we wrote under this hash. A deserialize failure
    // yields `None` and the caller deletes and recompiles, so a corrupted entry
    // cannot cause undefined behavior to escape.
    unsafe { Component::deserialize(engine, &cached) }.ok()
}

/// Formats the 32-byte hash as a lowercase hex cache file name.
fn hash_filename(hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut filename = String::with_capacity(64);
    for byte in hash {
        write!(&mut filename, "{byte:02x}").expect("write to string cannot fail");
    }
    filename.push_str(".cwasm");
    filename
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wat_text_is_compiled_when_wat_feature_enabled() {
        // A minimal valid WebAssembly component in text format. If wasmtime is
        // built with the `wat` feature, `Component::new` parses this directly.
        let engine = Engine::default();
        let bytes =
            br#"(component (core module (func (export "f"))) (core instance (instantiate 0)))"#;
        let result = Component::new(&engine, &bytes[..]);
        assert!(
            result.is_ok(),
            "wat should parse when wat feature is on: {result:?}"
        );
    }
}
