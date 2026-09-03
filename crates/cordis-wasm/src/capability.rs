use crate::WasmHostError;
use std::path::{Path, PathBuf};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder};

/// Filesystem access explicitly granted to one guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasiPreopen {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub writable: bool,
}

impl WasiPreopen {
    pub fn read_only(host_path: impl Into<PathBuf>, guest_path: impl Into<String>) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            writable: false,
        }
    }

    pub fn read_write(host_path: impl Into<PathBuf>, guest_path: impl Into<String>) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            writable: true,
        }
    }
}

/// `WASIp2` policy. Its default grants no ambient process, filesystem, or network access.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WasiCapabilities {
    preopens: Vec<WasiPreopen>,
}

impl WasiCapabilities {
    pub fn deny_all() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_preopen(mut self, preopen: WasiPreopen) -> Self {
        self.preopens.push(preopen);
        self
    }

    pub fn preopens(&self) -> &[WasiPreopen] {
        &self.preopens
    }

    pub(crate) fn build(&self) -> Result<WasiState, WasmHostError> {
        let mut builder = WasiCtxBuilder::new();
        for preopen in &self.preopens {
            validate_guest_path(&preopen.guest_path)?;
            let canonical =
                preopen
                    .host_path
                    .canonicalize()
                    .map_err(|error| WasmHostError::Capability {
                        message: format!(
                            "cannot canonicalize preopen {}: {error}",
                            preopen.host_path.display()
                        ),
                    })?;
            let perms = if preopen.writable {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };
            builder
                .preopened_dir(&canonical, &preopen.guest_path, perms)
                .map_err(WasmHostError::Engine)?;
        }
        Ok(WasiState {
            context: builder.build(),
            table: ResourceTable::new(),
        })
    }
}

fn validate_guest_path(path: &str) -> Result<(), WasmHostError> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(WasmHostError::Capability {
            message: format!(
                "guest preopen path must be relative and cannot contain `..`: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

pub(crate) struct WasiState {
    pub context: WasiCtx,
    pub table: ResourceTable,
}

impl std::fmt::Debug for WasiState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WasiState").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_wasi_policy_has_no_preopens() {
        let policy = WasiCapabilities::deny_all();
        assert!(policy.preopens().is_empty());
        policy.build().unwrap();
    }

    #[test]
    fn guest_preopen_rejects_parent_traversal() {
        let policy =
            WasiCapabilities::deny_all().with_preopen(WasiPreopen::read_only(".", "../escape"));
        assert!(matches!(
            policy.build(),
            Err(WasmHostError::Capability { .. })
        ));
    }
}
