use super::*;
use nodecraft_types::{AssetRef, MediaMeta};
const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><path d="M0 0 L200 0 L100 100 Z" fill="#339966"/></svg>"##;

#[test]
fn recraft35_svg_references_require_full_admission_even_without_header_claims() {
    let mut store = AssetStore::default();
    for bytes in [SVG, b"<svg xmlns='http://www.w3.org/2000/svg'><script>bad()</script></svg>", b"<html>not an SVG</html>"] {
        let asset = AssetRef { hash: store.insert(bytes).unwrap(), mime: "image/svg+xml".parse().unwrap(), meta: MediaMeta::default() };
        assert_eq!(store.validate_reference(&asset).is_ok(), bytes == SVG);
    }
    let mut asset = AssetRef {
        hash: store.insert(SVG).unwrap(),
        mime: "image/svg+xml".parse().unwrap(),
        meta: MediaMeta { width: Some(200), height: Some(100), ..Default::default() },
    };
    store.validate_reference(&asset).unwrap();
    let read = store.preview_read(&asset).unwrap();
    read.validate_image(SVG).unwrap();
    asset.meta.width = Some(199);
    assert!(store.validate_reference(&asset).is_err());
    asset.meta.width = Some(200);
    asset.meta.bits_per_channel = Some(8);
    assert!(store.validate_reference(&asset).is_err(), "vector original must not claim stored raster channels");
    assert_eq!(export::extension_for("image/svg+xml").as_deref(), Some("svg"));
}
