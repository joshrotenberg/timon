//! timon: a self-contained agent harness.
//!
//! One binary embedding Lua 5.4 over agent, session and tool APIs. See `docs/design.md`.

pub mod dirs;
pub mod docs;
pub mod error;
mod fennel;
pub mod layers;
pub mod modules;
pub mod options;
pub mod plugin;
pub mod runtime;
mod settings;
pub mod skills;
pub mod suggest;
pub mod value;

mod strict;

pub use error::{Error, Failure};
pub use runtime::Runtime;
