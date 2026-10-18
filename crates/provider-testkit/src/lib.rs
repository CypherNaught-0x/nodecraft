#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Provider-only test fixtures. Workspace consumers may depend on this crate only for tests.
//! Provider unit tests use their local `crate::mock` and local trait instead of this facade.
//! No generation kinds, run engine, clocks or provider adapter defaults are introduced here.

pub use nodecraft_providers as providers;
pub use nodecraft_providers::mock;
pub use nodecraft_types as types;
