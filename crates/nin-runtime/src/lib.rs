//! NIN runtime: values, interpreter, object model, builtins, modules.

pub mod builtins;
pub mod classes;
pub mod interp;
pub mod modules;
pub mod value;

pub use modules::norm_key;
pub use interp::{error_parts, parse_duration_ms, parse_size, Exec, Flow, Frame, Interp, InterpBuilder, Sink};
pub use value::*;
