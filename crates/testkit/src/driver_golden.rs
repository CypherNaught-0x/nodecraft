//! Driver-based fixture execution. No live provider, serial execution fallback or automatic blessing.
mod bounds;
mod controller;
mod provider;
mod synthetic;
pub use synthetic::SyntheticFixture;
pub mod runtime;
pub use controller::*;
pub use provider::{AdapterFixture, Calls, Gate, Gated};
