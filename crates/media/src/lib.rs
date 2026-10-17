#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft media: media sniffing, decode/encode, thumbnails and metadata (standalone).
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//!
//! Every function here takes untrusted bytes (pasted, dropped, downloaded from a provider or read
//! from a project file) and returns a [`MediaError`] rather than panicking. Decoding is capped by
//! [`Limits`]: input size, width, height and the decoder's allocations.

mod error;
mod hash;
mod image_io;
mod sniff;
mod thumb;

pub use error::MediaError;
pub use hash::ContentHash;
pub use image_io::{ColorModel, ImageMeta, Limits, decode_image, encode_png, probe_image};
pub use sniff::{MediaKind, Sniffed, sniff};
pub use thumb::{Thumbnail, ThumbnailCache, fit_within, thumbnail};

/// The `image` crate this API is built on, so callers use the same version of [`image::DynamicImage`].
pub use image;
