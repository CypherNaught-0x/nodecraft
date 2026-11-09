//! What a ComfyUI loader may be sent, and under what name (#110). Shared by the local server and
//! Comfy Cloud: both name an upload by its content and pick the extension the loaders read by.
use nodecraft_comfy::Media;
use nodecraft_media::MediaKind;

/// The usual extension for an upload of type `mime`; ComfyUI's loaders pick their decoder by it,
/// and list only the files whose extension they read.
pub(crate) fn extension(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/x-wav" | "audio/wave" => "wav",
        "audio/mp4" => "m4a",
        "model/gltf-binary" => "glb",
        _ => mime.rsplit('/').next().unwrap_or("bin").split(['+', ';', '.']).next().filter(|e| !e.is_empty()).unwrap_or("bin"),
    }
}

/// Refuses a file the loader at `target` can't read: an image loader takes images, a video
/// loader videos (and animated GIF or WebP, which video loaders read as frames), an audio loader
/// sounds (and videos, whose sound it reads), a 3D loader models. Text and documents never go.
pub(crate) fn check_kind(target: &str, loader: Media, bytes: &[u8], mime: &str) -> Result<(), String> {
    if mime == nodecraft_media::svg::MIME || nodecraft_media::svg::recognizes(bytes) {
        return Err(format!("{target} needs raster media; connect a Rasterize SVG action before this loader"));
    }
    let kind = nodecraft_media::sniff(bytes).map(|s| s.kind);
    let fits = match (loader, kind) {
        (Media::Image, Some(MediaKind::Image)) | (Media::Audio, Some(MediaKind::Audio | MediaKind::Video)) | (Media::Mesh, Some(MediaKind::Model3d)) => true,
        (Media::Video, Some(MediaKind::Video)) => true,
        (Media::Video, Some(MediaKind::Image)) => matches!(mime, "image/gif" | "image/webp"),
        _ => false,
    };
    if fits {
        return Ok(());
    }
    let wanted = match loader {
        Media::Image => "an image",
        Media::Video => "a video",
        Media::Audio => "a sound",
        Media::Mesh => "a 3D model",
    };
    Err(format!("{target} takes {wanted}, not {mime}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_are_named_with_the_extension_comfy_s_loaders_expect() {
        for (mime, ext) in [
            ("image/png", "png"),
            ("image/jpeg", "jpg"),
            ("video/mp4", "mp4"),
            ("video/quicktime", "mov"),
            ("video/x-matroska", "mkv"),
            ("video/webm", "webm"),
            ("audio/mpeg", "mp3"),
            ("audio/wav", "wav"),
            ("audio/flac", "flac"),
            ("audio/ogg", "ogg"),
            ("audio/mp4", "m4a"),
        ] {
            assert_eq!(extension(mime), ext, "{mime}");
        }
    }

    #[test]
    fn a_loader_takes_only_what_it_reads() {
        let png = nodecraft_media::encode_png(1, 1, &[0; 4], &Default::default()).unwrap();
        let wav = [b"RIFF\x24\0\0\0WAVEfmt ".as_slice(), &[0; 48]].concat();
        let gif = b"GIF89a\x01\0\x01\0\0\0\0;".to_vec();
        assert!(check_kind("1.image", Media::Image, &png, "image/png").is_ok());
        assert!(check_kind("2.audio", Media::Audio, &wav, "audio/wav").is_ok());
        assert!(check_kind("3.video", Media::Video, &gif, "image/gif").is_ok(), "video loaders read animated GIFs");
        assert_eq!(check_kind("1.image", Media::Image, &wav, "audio/wav").unwrap_err(), "1.image takes an image, not audio/wav");
        assert!(check_kind("3.video", Media::Video, &png, "image/png").is_err());
        assert!(check_kind("2.audio", Media::Audio, b"plain words", "text/plain").is_err());
    }
}
