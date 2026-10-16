//! timon: a self-contained agent harness.
//!
//! One binary embedding Lua 5.4 over agent, session and tool APIs. See `docs/design.md`.

pub mod error;
pub mod runtime;
pub mod suggest;
pub mod value;

mod strict;

pub use error::{Error, Failure};
pub use runtime::Runtime;
