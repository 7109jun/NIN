//! # nin-pkg: the NIN package manager core
//!
//! * [`archive`]: the `.nnpkg` container (format documented there), pack/extract.
//! * [`manifest`]: `nin-package.toml`.
//! * [`registry`]: local-directory and plain-`http://` registries, `index.toml`.
//! * [`resolver`]: backtracking dependency resolver.
//! * [`manager`]: [`PackageManager`] (install / remove / update / search / list).
//!
//! On-disk layout: `<root>/<name>/<version>/...` plus `<root>/installed.toml`.
//! Registries are directories holding `*.nnpkg` files; `publish` maintains an
//! `index.toml` next to them (it is only a cache: it is ignored when stale).

pub mod archive;
pub mod crc;
pub mod error;
pub mod manager;
pub mod manifest;
pub mod registry;
pub mod resolver;
#[doc(hidden)]
pub mod testutil;

pub use archive::{
    extract, pack, pack_with, parse_archive, read_archive, write_archive, Archive, Compression,
};
pub use error::{PkgError, Result};
pub use manager::{
    InstalledPkg, PackageManager, RemoveReport, SearchHit, UpdateReport,
};
pub use manifest::Manifest;
pub use registry::Source;
pub use resolver::ResolvedPkg;

/// Parse an archive held in memory (never panics on malformed input).
pub fn read_archive_bytes(bytes: &[u8]) -> Result<Archive> {
    parse_archive(bytes)
}
