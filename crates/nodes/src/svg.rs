//! Explicit vector-to-raster conversion: original SVG assets are never silently overwritten.
use crate::action::{self, ActionKind};
use nodecraft_types::Modality;

pub const RASTERIZE: &str = "image.rasterize";
pub fn kind() -> ActionKind {
    action::kind(
        RASTERIZE,
        "Rasterize SVG",
        vec![action::port("image", Modality::Image, false)],
        vec![action::port("out", Modality::Image, false)],
        vec![action::count("edge", "Longest edge (pixels)", 1, 2048, 1024)],
    )
}
