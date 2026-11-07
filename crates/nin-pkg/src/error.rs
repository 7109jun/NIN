//! Error type for the package manager.

use std::fmt;
use std::path::Path;

#[derive(Debug)]
pub enum PkgError {
    /// I/O failure, with the path involved.
    Io { path: String, source: std::io::Error },
    /// Invalid `nin-package.toml`.
    Manifest(String),
    /// Invalid / corrupt `.nnpkg` archive.
    Archive(String),
    /// Registry / index problem (including network errors).
    Registry(String),
    /// Package name is unknown to every registry.
    NotFound { name: String, required_by: String },
    /// Unsatisfiable version requirements.
    Conflict(String),
    /// Circular dependency; the vector is the cycle path (first == last).
    Cycle(Vec<String>),
    /// Resolver gave up (search space too large).
    Resolve(String),
    /// Package is not installed.
    NotInstalled(String),
    /// Removal refused because other packages depend on it.
    Dependents { name: String, dependents: Vec<String> },
    /// Bad install spec (`name@req`), or bad name/version.
    InvalidSpec(String),
    /// Publishing a different archive under an existing name+version.
    AlreadyPublished(String),
    /// Corrupt installed.toml.
    State(String),
}

impl PkgError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> PkgError {
        PkgError::Io { path: path.display().to_string(), source }
    }
}

impl fmt::Display for PkgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PkgError::Io { path, source } => write!(f, "I/O error on `{path}`: {source}"),
            PkgError::Manifest(m) => write!(f, "invalid manifest: {m}"),
            PkgError::Archive(m) => write!(f, "invalid .nnpkg archive: {m}"),
            PkgError::Registry(m) => write!(f, "registry error: {m}"),
            PkgError::NotFound { name, required_by } => write!(
                f,
                "package `{name}` was not found in any registry (required by {required_by}); \
                 check the name or add a registry"
            ),
            PkgError::Conflict(m) => write!(f, "dependency conflict: {m}"),
            PkgError::Cycle(c) => write!(f, "circular dependency: {}", c.join(" -> ")),
            PkgError::Resolve(m) => write!(f, "dependency resolution failed: {m}"),
            PkgError::NotInstalled(n) => write!(f, "package `{n}` is not installed"),
            PkgError::Dependents { name, dependents } => write!(
                f,
                "cannot remove `{name}`: required by installed package(s) {}; use force to override",
                dependents.join(", ")
            ),
            PkgError::InvalidSpec(m) => write!(f, "invalid package spec: {m}"),
            PkgError::AlreadyPublished(m) => write!(f, "already published with different content: {m}"),
            PkgError::State(m) => write!(f, "corrupt installed.toml: {m}"),
        }
    }
}

impl std::error::Error for PkgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PkgError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, PkgError>;
