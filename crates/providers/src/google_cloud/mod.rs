//! Google Cloud (#742): service-account credentials and the models served on Google Cloud's
//! Gemini Enterprise Agent Platform (formerly Vertex AI). A service account's key signs a
//! short-lived JWT, which Google's token endpoint exchanges, free, for an hour's access token.
mod credential;
#[cfg(test)]
mod tests;
mod token;
#[cfg(feature = "google-tts")]
pub mod tts;
#[cfg(feature = "google-veo")]
pub mod veo;
pub use credential::{ACCOUNT, ServiceAccount, TOKEN_URI, compact};
pub use token::{Clock, Tokens, system_clock};
