//! timon: a task runner in one self-contained binary, with agents built in.
//!
//! Tasks and scripts in Lua 5.4 and Fennel over a toolbox and agent and session APIs. See
//! `docs/design.md`.

pub mod agent;
pub mod agents;
pub mod budget;
pub mod complete;
pub mod dirs;
pub mod docs;
pub mod error;
mod fennel;
pub mod flags;
pub mod layers;
pub mod modules;
pub mod options;
pub mod plugin;
pub mod redact;
pub mod runtime;
pub mod scripts;
pub mod serve;
pub mod sessions;
mod settings;
pub mod skills;
pub mod suggest;
pub mod tasks;
pub mod types;
pub mod value;

mod strict;

pub use error::{Error, Failure};
pub use runtime::Runtime;
