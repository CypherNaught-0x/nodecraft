#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft media: media sniffing, decode/encode, thumbnails, meshes and metadata (standalone).
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//!
//! Every function here takes untrusted bytes (pasted, dropped, downloaded from a provider or read
//! from a project file) and returns a [`MediaError`] rather than panicking. Decoding is capped by
//! [`Limits`]: input size, width, height and the decoder's allocations.

mod error;
mod hash;
mod image_io;
mod matroska;
mod mesh;
mod movie;
mod sniff;
mod sound;
pub mod style;
mod thumb;

pub use error::MediaError;
pub use hash::ContentHash;
pub use image_io::{
    ColorModel, ColorTag, ImageMeta, Limits, MAX_IMAGE_PROBE_BYTES, decode_image, encode_png, is_undecoded_image, probe_image, probe_image_reader,
};
pub use mesh::{Bounds, Mesh, MeshFormat, MeshInfo, MeshLimits, decode_mesh, decode_mesh_as, encode_glb, encode_obj, probe_mesh};
pub use movie::{MovieInfo, probe_movie};
pub use sniff::{MediaKind, Sniffed, sniff};
pub use sound::{SoundInfo, probe_sound};
pub use thumb::{Thumbnail, ThumbnailCache, fit_within, thumbnail};

/// The `image` crate this API is built on, so callers use the same version of [`image::DynamicImage`].
pub use image;

/// PDF documents: page count, pages as images, page text (#113).
#[cfg(feature = "pdf")]
mod document;
#[cfg(feature = "fixtures")]
pub use document::fixture as pdf_fixture;
#[cfg(feature = "pdf")]
pub use document::{Document, DocumentInfo, DocumentLimits, PageImage, PageRegion, Reader, probe_document};

/// Bounded native FFmpeg service (external tools are never bundled).
#[cfg(feature = "video")]
pub mod video;
