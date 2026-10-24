//! Structural scene contracts: no GPU, provider, fabricated result or screenshot dependency.
use std::collections::BTreeSet;

use crate::scene::{self, Fail, node_id};

#[test]
fn three_plates_branch_and_recombine_in_coloured_fields() -> Result<(), Fail> {
    let plates = crate::art::plates()?;
    let stage = scene::showcase(&plates)?;
    let canvas = stage.app.session.document().canvases().get(&stage.app.canvas).ok_or_else(|| scene::fail("missing canvas"))?;
    assert_eq!(canvas.fields().len(), 3);
    let colors: BTreeSet<_> = canvas.fields().values().filter_map(|field| field.color.as_deref()).collect();
    assert_eq!(colors.len(), 3);
    for (field, members) in [(1, [5, 6, 7, 8]), (2, [17, 18, 16, 21]), (3, [19, 20, 22, 23])] {
        for key in members {
            let node = canvas.nodes().get(&node_id(key)).ok_or_else(|| scene::fail("missing field member"))?;
            let id = nodecraft_types::FieldId::from_bytes([field; 16]);
            assert_eq!(node.field, Some(id));
            let zone = canvas.fields().get(&id).ok_or_else(|| scene::fail("missing field"))?;
            let descriptor = stage.app.session.kinds().get(&node.kind).map_err(|error| scene::fail(error.message))?.descriptor();
            let tokens = stage.app.theme.tokens();
            let size = nodecraft_ui_egui::card::world_size(node, Some(descriptor), tokens);
            // An uncomputed action will acquire a preview and one history row. Leave room for
            // those future bounds, not just today's empty card; no fake history is inserted.
            let growth = if node.kind.starts_with("image.") { tokens.card_preview_height + tokens.control_row_height } else { 0.0 };
            assert!(node.pos.x >= zone.rect.pos.x && node.pos.x + size.x <= zone.rect.pos.x + zone.rect.width);
            assert!(node.pos.y >= zone.rect.pos.y + 48.0 && node.pos.y + size.y + growth <= zone.rect.pos.y + zone.rect.height);
        }
    }
    let mut hashes = BTreeSet::new();
    for key in [5, 17, 19] {
        let node = canvas.nodes().get(&node_id(key)).ok_or_else(|| scene::fail("missing plate"))?;
        assert_eq!(node.kind, "source.image");
        let image = nodecraft_engine::split::image_of(node).ok_or_else(|| scene::fail("plate has no real imported image"))?;
        hashes.insert(image.hash);
    }
    assert_eq!(hashes.len(), 3, "all three original plates must actually be used");
    let edges: Vec<_> = canvas.edges().values().collect();
    for (from, to, port) in
        [(6, 7, "image"), (6, 16, "image"), (16, 21, "image"), (18, 21, "overlay"), (21, 22, "image"), (20, 22, "overlay"), (22, 23, "image")]
    {
        assert!(edges.iter().any(|edge| edge.from.node == node_id(from) && edge.to.node == node_id(to) && edge.to.port == port));
    }
    assert!(canvas.nodes().values().all(|node| !node.kind.starts_with("generate.") && node.kind != "source.video" && node.kind != "source.audio"));
    // Histories on actions must come from live computation, not fixture injection.
    for key in [6, 7, 8, 16, 18, 20, 21, 22, 23] {
        let node = canvas.nodes().get(&node_id(key)).ok_or_else(|| scene::fail("missing action"))?;
        assert!(node.history.is_empty());
        assert!(edges.iter().any(|edge| edge.to.node == node.id));
    }
    Ok(())
}

#[test]
fn shots_keep_original_names_and_readable_hero() -> Result<(), Fail> {
    let stage = scene::showcase(&crate::art::plates()?)?;
    let canvas = stage.app.session.document().canvases().get(&stage.app.canvas).ok_or_else(|| scene::fail("missing canvas"))?;
    let shots = crate::shots::shots();
    let names: BTreeSet<_> = shots.iter().map(|shot| shot.name).collect();
    assert_eq!(names.len(), shots.len());
    for name in [
        "hero",
        "detail-grade",
        "detail-chain",
        "detail-ports",
        "palette",
        "tags",
        "comments",
        "modality",
        "notes",
        "chrome",
        "light",
        "detail-fields",
        "detail-composite",
    ] {
        assert!(names.contains(name), "missing shot {name}");
    }
    for shot in &shots {
        for key in shot.frame.iter().chain(shot.wait_results) {
            assert!(canvas.nodes().contains_key(&node_id(*key)), "{} references missing card {key}", shot.name);
        }
        if shot.name == "modality" {
            let (x, y, width, height) = shot.frame_rect.ok_or_else(|| scene::fail("routing explainer needs explicit framing"))?;
            let bounds = egui::Rect::from_min_size(egui::Pos2::new(x, y), egui::Vec2::new(width, height));
            for key in shot.frame {
                let node = canvas.nodes().get(&node_id(*key)).ok_or_else(|| scene::fail("missing routed card"))?;
                let descriptor = stage.app.session.kinds().get(&node.kind).map_err(|error| scene::fail(error.message))?.descriptor();
                let rect = nodecraft_ui_egui::card::world_rect(node, Some(descriptor), stage.app.theme.tokens()).expand2(egui::Vec2::new(26.0, 42.0));
                assert!(bounds.contains_rect(rect), "routing camera must include the full imported-image preview");
            }
            let zoom = ((shot.size.0 - shot.padding * 2.0) / width).min((shot.size.1 - 80.0 - shot.padding * 2.0) / height);
            assert!(zoom > nodecraft_ui_egui::card::BODY_MIN_ZOOM);
        }
        if shot.palette.is_some() {
            assert!(shot.palette_entry.is_some(), "palette needs evidence of its expected result row");
            let at = egui::Pos2::new(shot.palette_at.0, shot.palette_at.1);
            for field in canvas.fields().values() {
                let rect = egui::Rect::from_min_size(egui::Pos2::new(field.rect.pos.x, field.rect.pos.y), egui::Vec2::new(field.rect.width, field.rect.height));
                assert!(!rect.contains(at), "palette anchor must not double-click a field/header");
            }
            for node in canvas.nodes().values() {
                let descriptor = stage.app.session.kinds().get(&node.kind).map_err(|error| scene::fail(error.message))?.descriptor();
                assert!(
                    !nodecraft_ui_egui::card::world_rect(node, Some(descriptor), stage.app.theme.tokens()).contains(at),
                    "palette anchor must be empty canvas"
                );
            }
        }
        if matches!(shot.name, "hero" | "light") {
            assert!(shot.select.is_empty(), "hero must not open incompatible shared parameters");
            let (_, _, width, height) = shot.frame_rect.ok_or_else(|| scene::fail("hero needs explicit framing"))?;
            let zoom = ((shot.size.0 - shot.padding * 2.0) / width).min((shot.size.1 - 80.0 - shot.padding * 2.0) / height);
            assert!(zoom > nodecraft_ui_egui::card::BODY_MIN_ZOOM);
            assert_eq!(shot.frame.len(), 12);
            assert_eq!(shot.wait_results.len(), 9);
        }
    }
    Ok(())
}

#[test]
fn missing_plates_are_an_actionable_error() {
    assert!(scene::showcase(&[]).is_err());
}
