//! One-line summaries of the built-in node kinds (#1121): what a kind makes or does, for the
//! quick-add palette and any other surface that lists kinds. Plain sentence case, no full stop,
//! short enough for one line beside the label. A kind without one (a plugin's action) has none;
//! surfaces fall back to its ports.
use crate::NodeDescriptor;

impl NodeDescriptor {
    /// The kind's one-line summary, if it is a built-in kind.
    pub fn summary(&self) -> Option<&'static str> {
        summary(&self.id)
    }
}

/// The one-line summary of the built-in kind `id`.
pub fn summary(id: &str) -> Option<&'static str> {
    Some(match id {
        "source.text" => "Text you write or paste",
        "source.image" => "An image you upload or drop in",
        "source.video" => "A video you upload or drop in",
        "source.audio" => "A sound file you upload or drop in",
        "source.mesh" => "A 3D model you upload or drop in",
        "source.document" => "A PDF you upload or drop in",
        "source.folder" => "The files of a folder, filtered and sorted",
        "generation.text" => "Writes text with a language model",
        "generation.image" => "Makes an image from a prompt and references",
        "generation.video" => "Makes a video from a prompt or an image",
        "generation.audio" => "Makes speech, music or sound from text",
        "generation.mesh" => "Makes a 3D model from a prompt or an image",
        "generation.style" => "Trains a style from example images",
        "generation.comfy_workflow" => "Runs a ComfyUI workflow you import",
        "structure.note" => "A note on the canvas; never runs",
        "structure.router" => "Passes one set of inputs on to many cards",
        "structure.batch" => "Runs the cards after it once per item",
        "structure.deck" => "Slides that show what canvas cards show",
        "structure.technique" => "A published Technique as one card",
        "media.export" => "Writes the media wired in to a folder",
        "element" => "A reusable subject a prompt can mention by name",
        "document.pages" => "Renders a PDF's pages as images",
        "document.text" => "Reads the text out of a PDF",
        "text.concat" => "Joins the connected texts into one",
        "text.replace" => "Finds and replaces words in text",
        "text.split" => "Splits text into a list of parts",
        "text.template" => "Fills a template with the connected texts",
        "image.blur" => "Softens an image with a Gaussian blur",
        "image.grade" => "Adjusts brightness, contrast and colour",
        "image.filter" => "Grayscale, sepia or noir look",
        "image.tint" => "Washes an image with one colour",
        "image.resize" => "Scales an image to a new size",
        "image.crop" => "Cuts an image down to a region",
        "image.rotate" => "Turns an image by any angle",
        "image.flip" => "Mirrors an image across or down",
        "image.key" => "Makes one colour transparent",
        "image.replace_color" => "Swaps one colour for another",
        "image.aspect" => "Crops or pads to an aspect ratio",
        "image.overlay" => "Places one image over another",
        "image.expand" => "Adds transparent margins for outpainting",
        "image.duplicate" => "The same image, several times",
        "image.mask" => "A brush mask over an image, for inpainting",
        "image.layers" => "Composes text, shapes and images in layers",
        "image.rasterize" => "Turns an SVG into pixels",
        "image.shader" => "Runs a shader you write over an image",
        "mesh.convert" => "Writes a 3D model in another format",
        "mesh.photo" => "A picture of a 3D model from one camera",
        "mesh.turntable" => "A video of a 3D model turning round",
        "mesh.views" => "One image of 4, 8 or 16 views round a model",
        "video.trim" => "Keeps a stretch of a video",
        "video.speed" => "Plays a video faster or slower",
        "video.stitch" => "Joins videos one after another",
        "video.replace_audio" => "Puts a different sound under a video",
        "video.split" => "Cuts a video into scenes or equal parts",
        "video.timeline" => "Edits clips, sound and captions on tracks",
        "video.frames" => "Pictures taken from across a video",
        "video.contact_sheet" => "Frames from a video laid out in a grid",
        "video.long_exposure" => "Frames blended into one picture",
        "video.reverse" => "Plays a video backwards, or there and back",
        "video.ken_burns" => "A slow pan and zoom over a still",
        "video.extract_audio" => "Takes the sound out of a video",
        "video.grade" => "Adjusts a video's brightness and colour",
        "video.filter" => "One effect over a whole video",
        "video.key" => "Keys a colour out of a video",
        "video.watermark" => "Puts an image over a video as a mark",
        "video.duplicate" => "The same video, several times",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::Registry;

    #[test]
    fn every_built_in_kind_has_a_short_summary() {
        let registry = Registry::builtins().unwrap();
        for descriptor in registry.descriptors() {
            let summary = descriptor.summary().unwrap_or_else(|| panic!("{} has no summary", descriptor.id));
            assert!(!summary.is_empty() && summary.chars().count() <= 60, "{}: {summary:?}", descriptor.id);
            assert!(!summary.ends_with('.'), "{}: one line, no full stop", descriptor.id);
        }
        assert!(super::summary("plugin.example").is_none());
    }
}
