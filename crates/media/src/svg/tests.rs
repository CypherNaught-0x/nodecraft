use super::*;

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><defs><linearGradient id="g"><stop stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/></linearGradient></defs><rect width="200" height="100" fill="url(#g)"/><circle cx="100" cy="50" r="25" fill="#00ff00" fill-opacity="0.5"/></svg>"##;
fn document(body: &str) -> Vec<u8> {
    format!("<svg xmlns=\"{NS}\" width=\"100\" height=\"100\">{body}</svg>").into_bytes()
}

#[test]
fn recraft35_svg_is_typed_and_preview_keeps_intrinsic_geometry_and_straight_alpha() {
    assert_eq!(crate::sniff(SVG).unwrap().mime, MIME);
    let info = probe(SVG, &Default::default()).unwrap();
    assert_eq!((info.width, info.height, info.preview_unavailable), (200, 100, None));
    let image = render(SVG, 128, &Default::default()).unwrap();
    assert_eq!((image.width, image.height, image.source_width, image.source_height), (128, 64, 200, 100));
    assert_eq!(image.rgba.len(), 128 * 64 * 4);
    assert!(image.rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[0] > 180));
    let alpha = document("<rect width=\"100\" height=\"100\" fill=\"red\" opacity=\"0.5\"/>");
    let image = render(&alpha, 32, &Default::default()).unwrap();
    assert_eq!(image.rgba[0], 255, "renderer premultiplication must be undone");
    assert!((126..=129).contains(&image.rgba[3]));
    assert!(crate::decode_image(SVG, &Default::default()).is_err(), "raster consumers need explicit conversion");
}

#[test]
fn recraft35_bom_declaration_comments_and_prefixed_roots_are_recognized_without_html_confusion() {
    let input = b"\xef\xbb\xbf<?xml version=\"1.0\"?><!--doc--><s:svg xmlns:s=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 6\"/>";
    assert!(recognizes(input));
    assert_eq!(probe(input, &Default::default()).unwrap().width, 8);
    assert!(!recognizes(b"<html><svg/></html>"));
    let long_prologue = format!("<!--{}--><svg xmlns=\"{NS}\" width=\"8\" height=\"6\"/>", "x".repeat(70_000));
    assert!(recognizes(long_prologue.as_bytes()));
    assert_eq!(crate::sniff(long_prologue.as_bytes()).unwrap().mime, MIME);
    assert!(probe(long_prologue.as_bytes(), &Default::default()).is_ok());
    assert!(probe(b"<svg width=\"8\" height=\"6\"/>", &Default::default()).is_err());
    assert_eq!(crate::sniff(b"a prompt mentioning <svg/> is prose").unwrap().mime, "text/plain");
}

#[test]
fn recraft35_scripts_entities_external_resources_and_css_escape_tricks_are_refused() {
    for body in [
        "<script>alert(1)</script>",
        "<rect onload=\"bad()\"/>",
        "<rect ONCLICK=\"bad()\"/>",
        "<foreignObject><div/></foreignObject>",
        "<animate attributeName=\"href\" values=\"https://example.invalid\"/>",
        "<image href=\"file:///private/no-read.png\"/>",
        "<image href=\"https://example.invalid/no-fetch.png\"/>",
        "<image href=\"data:image/svg+xml;base64,AA==\"/>",
        "<rect style=\"fill:url(https://example.invalid/no-fetch)\"/>",
        "<rect style=\"fill:u\\72l(#x)\"/>",
        "<style>@import 'https://example.invalid';</style>",
        "<style>safe<!-- split -->@import 'https://example.invalid';</style>",
        "<style><![CDATA[@font-face{src:url(file:///private/no-read)}]]></style>",
        "<image href=\"&#104;ttps://example.invalid/no-fetch\"/>",
        "<g xml:base=\"file:///private/\"/>",
    ] {
        assert!(probe(&document(body), &Default::default()).is_err(), "unsafe {body}");
    }
    for bytes in [
        b"<!DOCTYPE svg [<!ENTITY e 'hi'>]><svg xmlns='http://www.w3.org/2000/svg'>&e;</svg>".as_slice(),
        b"<?xml-stylesheet href='https://example.invalid'?><svg xmlns='http://www.w3.org/2000/svg'/>",
        b"\x1f\x8bcompressed SVGZ",
    ] {
        assert!(probe(bytes, &Default::default()).is_err());
    }
}

#[test]
fn recraft35_bounded_static_features_keep_the_useful_original_when_preview_cannot_render_them() {
    for body in [
        "<text x=\"5\" y=\"20\">Typography stays in the original</text>",
        "<text style=\"font-family:未知;fill:red !important\">文字</text>",
        "<filter id=\"f\"><feGaussianBlur stdDeviation=\"2\"/></filter><rect width=\"50\" height=\"50\" filter=\"url(#f)\"/>",
        "<pattern id=\"p\" width=\"10\" height=\"10\"><rect width=\"5\" height=\"5\"/></pattern>",
        "<rect width=\"50\" height=\"50\" style=\"filter:blur(2px) drop-shadow(0 1px 2px #000)\"/>",
        "<g style=\"transform:translate(2px,3px)\"><rect width=\"50\" height=\"50\"/></g>",
        "<style>.a{fill:red}</style><rect class=\"a\" width=\"100\" height=\"100\"/>",
    ] {
        let bytes = document(body);
        assert!(probe(&bytes, &Default::default()).unwrap().preview_unavailable.is_some());
        assert!(render(&bytes, 128, &Default::default()).is_err());
    }
}

#[test]
fn recraft35_reference_cycles_and_exponential_expansion_are_admitted_before_renderer() {
    for body in [
        "<g id=\"a\"><use href=\"#a\"/></g>",
        "<g id=\"a\"><use href=\"#b\"/></g><g id=\"b\"><use href=\"#a\"/></g>",
        "<linearGradient id=\"a\" href=\"#b\"/><linearGradient id=\"b\" href=\"#a\"/>",
    ] {
        assert!(probe(&document(body), &Default::default()).is_err());
    }
    let mut body = String::from("<g id=\"n0\"><rect width=\"1\" height=\"1\"/></g>");
    for n in 1..18 {
        body.push_str(&format!("<g id=\"n{n}\"><use href=\"#n{}\"/><use href=\"#n{}\"/></g>", n - 1, n - 1));
    }
    assert!(probe(&document(&body), &Default::default()).is_err());
    assert!(probe(&document("<defs><rect id=\"a\" width=\"10\" height=\"10\"/></defs><use href=\"#a\"/>"), &Default::default()).is_ok());
}

#[test]
fn recraft35_dimensions_structure_paths_encoded_size_and_render_scratch_have_independent_bounds() {
    for attr in ["width='0'", "width='NaN'", "width='inf'", "width='20000'", "viewBox='0 0 -1 10'"] {
        let svg = format!("<svg xmlns='{NS}' {attr}/>");
        assert!(probe(svg.as_bytes(), &Default::default()).is_err(), "{attr}");
    }
    assert!(probe(SVG, &Limits { max_input_bytes: SVG.len() as u64 - 1, ..Default::default() }).is_err());
    assert!(probe(SVG, &Limits { max_width: 199, ..Default::default() }).is_err());
    assert!(probe(SVG, &Limits { max_alloc: 1, ..Default::default() }).is_err());
    assert!(render(SVG, 2049, &Default::default()).is_err());
    assert!(render(SVG, 512, &Limits { max_alloc: 128 * 1024, ..Default::default() }).is_err());
    let deep = format!("{}{}", "<g>".repeat(65), "</g>".repeat(65));
    assert!(probe(&document(&deep), &Default::default()).is_err());
    let path = format!("<path d='M0 0 {}'/>", "L1 1 ".repeat(MAX_SEGMENTS));
    assert!(probe(&document(&path), &Default::default()).is_err());
    // At least the documented 1.72MiB output size is useful; don't confuse encoded bytes with
    // path complexity, or require a costly pixel allocation just to admit the original.
    let large = document(&format!("<!--{}--><rect width='100' height='100'/>", "x".repeat(1_800_000)));
    assert!(probe(&large, &Default::default()).is_ok());
    let oversized = vec![b'x'; MAX_BYTES as usize + 1];
    assert!(probe(&oversized, &Default::default()).is_err());
}

#[test]
fn recraft35_all_truncations_and_small_adversarial_bytes_are_fallible_without_panics() {
    for end in 0..SVG.len() {
        let bytes = &SVG[..end];
        let _ = probe(bytes, &Default::default());
        let _ = recognizes(bytes);
    }
    for byte in [0, 0xff, b'<', b'&', b'\0'] {
        for length in [0, 1, 32, 1024] {
            let _ = probe(&vec![byte; length], &Default::default());
        }
    }
}

#[test]
fn recraft35_css_admission_covers_resource_functions_properties_and_browser_grammar() {
    for style in [
        "background-image:image-set('//host/image' 1x)",
        "fill:image-set('//host/image' 1x)",
        "fill:URL(//host/image)",
        "fill:url(' #paint ')",
        "fill:var(--remote)",
        "src:'//host/font'",
        "color-profile:remote",
        "list-style-image:url(//host/image)",
        "fill:rgb(1,2,3) image-set('image.png' 1x)",
        "fill:cross-fade(url(#p),red,1)",
        "filter:blur(var(--x))",
        "filter:drop-shadow(0 1px url(//host/image))",
        "transform:translate(1e999px)",
        "font-family:foo;src:local(foo)",
        "--custom:url(//host/image)",
    ] {
        let svg = document(&format!("<rect style=\"{style}\"/>"));
        assert!(probe(&svg, &Default::default()).is_err(), "{style}");
    }
    for body in ["<font-face-uri href='#p'/>", "<feImage href='//host/image'/>", "<g src='//host/image'/>", "<rect color-profile='url(//host/profile)'/>"] {
        assert!(probe(&document(body), &Default::default()).is_err(), "{body}");
    }
    let safe = document(
        "<defs><linearGradient id='p'><stop stop-color='red'/></linearGradient></defs><rect width='50' height='50' style=\"fill:URL('#p') red;stroke:rgb(1, 2, 3);opacity:.5\"/>",
    );
    assert!(probe(&safe, &Default::default()).unwrap().preview_unavailable.is_none());
    assert!(render(&safe, 128, &Default::default()).is_ok());
    let safe_sheet = document("<style>.a > rect { fill:rgb(1, 2, 3);stroke-width:2 }</style><g class='a'><rect width='50' height='50'/></g>");
    assert!(probe(&safe_sheet, &Default::default()).unwrap().preview_unavailable.is_some());
}

#[test]
fn recraft35_expanded_segments_are_charged_before_normalization_and_safe_originals_survive_preview_caps() {
    let path = format!("<defs><path id='p' d='M0 0 {}'/></defs>", "L1 1 ".repeat(1000));
    let bounded = document(&format!("{path}{}", "<use href='#p'/>".repeat(33)));
    let info = probe(&bounded, &Default::default()).unwrap();
    assert!(info.preview_unavailable.is_some(), "source <32K segments but expanded paths exceed preview capacity");
    assert!(matches!(render(&bounded, 128, &Default::default()), Err(MediaError::UnsupportedFeature(_))));
    let excessive = document(&format!("{path}{}", "<use href='#p'/>".repeat(250)));
    assert!(probe(&excessive, &Default::default()).is_err(), "hard retained expansion budget");
    let clipped = document(&format!(
        "<defs><clipPath id='c'><path d='M0 0 {}'/></clipPath></defs>{}",
        "L1 1 ".repeat(1000),
        "<rect width='1' height='1' clip-path='url(#c)'/>".repeat(33)
    ));
    assert!(probe(&clipped, &Default::default()).unwrap().preview_unavailable.is_some());
    let dashed = document("<path d='M0 0 L100 100' stroke='black' stroke-dasharray='.0000000001 .0000000001'/>");
    assert!(probe(&dashed, &Default::default()).unwrap().preview_unavailable.is_some());
    assert!(render(&dashed, 128, &Default::default()).is_err());
}

#[test]
fn recraft35_lowered_document_allocation_budget_is_checked_before_dom_allocation() {
    let before = DOM_PARSE_CALLS.with(std::cell::Cell::get);
    assert!(probe(SVG, &Limits { max_alloc: 1, ..Default::default() }).is_err());
    let many_nodes = document(&"<g/>".repeat(1000));
    assert!(many_nodes.len() * 4 < 100_000);
    assert!(probe(&many_nodes, &Limits { max_alloc: 100_000, ..Default::default() }).is_err());
    let comments = document(&"<!--x-->".repeat(1000));
    assert!(comments.len() * 4 < 100_000);
    assert!(probe(&comments, &Limits { max_alloc: 100_000, ..Default::default() }).is_err());
    assert_eq!(DOM_PARSE_CALLS.with(std::cell::Cell::get), before, "caller-lowered limits precede DOM allocation for elements AND comments");
}

#[test]
fn recraft35_renderer_uses_admitted_default_viewport_and_keeps_fractional_aspect() {
    let svg = format!("<svg xmlns='{NS}'><rect width='30' height='30' fill='red'/></svg>");
    let info = probe(svg.as_bytes(), &Default::default()).unwrap();
    assert_eq!((info.width, info.height), (300, 150));
    let image = render(svg.as_bytes(), 120, &Default::default()).unwrap();
    assert_eq!((image.width, image.height), (120, 60));
    assert_eq!(image.rgba[(5 * 120 + 5) * 4 + 3], 255);
    assert_eq!(image.rgba[(5 * 120 + 20) * 4 + 3], 0, "a30-unit rect occupies12preview pixels, not36 from usvg's100-unit default");
    for attrs in ["width='3.2' height='1.2'", "width='100%' height='100%' viewBox='0 0 3.2 1.2'"] {
        let svg = format!("<svg xmlns='{NS}' {attrs}><rect width='1' height='1'/></svg>");
        let info = probe(svg.as_bytes(), &Default::default()).unwrap();
        assert_eq!((info.width, info.height), (4, 2));
        let image = render(svg.as_bytes(), 128, &Default::default()).unwrap();
        assert_eq!((image.width, image.height), (128, 48), "ceil metadata must not stretch fractional vector viewports");
    }
}

#[test]
fn recraft35_preview_root_copy_is_bounded_and_preserves_namespaces_escaped_attributes_and_original() {
    let original = b"\xef\xbb\xbf<?xml version='1.0'?><!--before--><s:svg xmlns:s='http://www.w3.org/2000/svg' width='8' height='6' data-note='a &quot; &gt; b'><s:rect width='4' height='3'/></s:svg>";
    let copy = viewport::explicit(original, f64::MIN_POSITIVE, 16_384.0).unwrap();
    assert!(copy.len() <= original.len() + 128, "scientific dimensions cannot expand to hundreds of zeroes");
    let text = std::str::from_utf8(&copy).unwrap();
    let doc = Document::parse(text).unwrap();
    assert_eq!(doc.root_element().tag_name().namespace(), Some(NS));
    assert_eq!(doc.root_element().attribute("data-note"), Some("a \" > b"));
    assert_eq!(doc.root_element().children().filter(Node::is_element).count(), 1);
    assert_eq!((probe(original, &Default::default()).unwrap().width, probe(original, &Default::default()).unwrap().height), (8, 6));
}

#[test]
fn recraft35_inert_root_quote_cannot_inject_preview_attributes() {
    let bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" data-note='x" stroke-dasharray=".0001 .0001'><path d="M0 0 L100 100" fill="none" stroke="black"/></svg>"##;
    assert!(probe(bytes, &Default::default()).unwrap().preview_unavailable.is_none());
    let normalized = viewport::explicit(bytes, 100.0, 100.0).unwrap();
    let text = std::str::from_utf8(&normalized).unwrap();
    let doc = Document::parse(text).unwrap();
    assert_eq!(doc.root_element().attribute("data-note"), Some("x\" stroke-dasharray=\".0001 .0001"), "inert quoted text remains one attribute");
    assert!(doc.root_element().attribute("stroke-dasharray").is_none(), "no preview-only work attribute");
    assert!(probe(&normalized, &Default::default()).unwrap().preview_unavailable.is_none());
    // Only render after the structural assertions, so the old unsafe code never executes
    // the injected ~707K dashes while proving the regression red.
    assert!(render(bytes, 128, &Default::default()).is_ok());
}

#[test]
fn recraft35_root_quotes_entities_and_namespace_values_survive_without_double_escaping() {
    let input = br##"<s:svg xmlns:s="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="100" height="100" data-note='literal " &quot; &#34; &amp; value' xlink:title='quoted " name' style='fill:url("#g")'><s:defs><s:linearGradient id="g"><s:stop stop-color="red"/></s:linearGradient></s:defs><s:rect width="100" height="100"/></s:svg>"##;
    let original = Document::parse(std::str::from_utf8(input).unwrap()).unwrap();
    let normalized = viewport::explicit(input, 100.0, 100.0).unwrap();
    let doc = Document::parse(std::str::from_utf8(&normalized).unwrap()).unwrap();
    assert_eq!(doc.root_element().tag_name().namespace(), Some(NS));
    assert_eq!(doc.root_element().attributes().len(), original.root_element().attributes().len());
    for name in ["data-note", "style"] {
        assert_eq!(doc.root_element().attribute(name), original.root_element().attribute(name));
    }
    assert_eq!(doc.root_element().attribute(("http://www.w3.org/1999/xlink", "title")), Some("quoted \" name"));
    assert!(render(input, 128, &Default::default()).is_ok());
}

#[test]
fn recraft35_escaped_root_extent_refuses_before_materialization_and_keeps_original_admitted() {
    let bytes = format!("<svg xmlns='{NS}' width='100' height='100' data-note='{}'><rect width='10' height='10'/></svg>", "\"".repeat(512));
    assert!(probe(bytes.as_bytes(), &Default::default()).unwrap().preview_unavailable.is_none(), "inert original remains useful");
    let before = viewport::ROOT_MATERIALIZATIONS.with(std::cell::Cell::get);
    assert!(viewport::explicit(bytes.as_bytes(), 100.0, 100.0).is_err());
    assert_eq!(viewport::ROOT_MATERIALIZATIONS.with(std::cell::Cell::get), before, "escaped root extent checked before owned document buffers");
    assert!(render(bytes.as_bytes(), 128, &Default::default()).is_err());
    assert!(probe(bytes.as_bytes(), &Default::default()).is_ok(), "failed preview never discards the original");
}
