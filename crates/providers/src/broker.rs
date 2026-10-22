//! The provider broker (#174): a plugin's provider is a declarative operation template the host
//! sends itself, never code that talks to the network. The template names one HTTPS origin, a
//! method and path per operation, a request body built only from the job's own parameters and
//! text, and the response fields the host may read; everything else is the host's: the
//! credential and where it goes, the transport and its bounds, the quote, the budget and the
//! idempotency key.
//!
//! - [`template`]: decodes and validates a template against the host's ceilings.
//! - [`plan`]: turns a job into the requests the template allows, and reads only the response
//!   fields it declares.
//!
//! A template is untrusted input from a package: every rule here is checked before anything is
//! sent, and a template that breaks one is refused whole.

pub mod plan;
pub mod template;

pub use plan::{authorize, plan_cancel, plan_poll, plan_submit, read_poll, read_submit};
pub use template::{Credential, Origin, Template, TemplateError};

#[cfg(test)]
mod tests;
