//! nin-plugin: the stable, C-ABI, JSON-based NIN plugin interface.
//!
//! * [`abi`]  - the `#[repr(C)]` contract shared by hosts and plugins.
//! * [`sdk`]  - plugin-side helpers and the [`export_plugin!`] macro (no libloading).
//! * `host`   - (feature `host`) the loader used by NIN Shell.

pub mod abi;
pub mod sdk;

#[cfg(feature = "host")]
pub mod host;

pub use abi::NIN_PLUGIN_ABI_VERSION;
pub use serde_json;
