//! Shots: a camera, some panels, one frame drawn by the product's own wgpu backend.
//!
//! Each shot rebuilds the same showcase document and points the camera somewhere different, so
//! the README gallery is one workflow seen from several distances. Nothing is painted here: the
//! renderer is `NodeCraftApp::show`, offscreen, through `egui_kittest`'s headless wgpu
//! (`without_display_handle`, Metal on macOS, a software adapter if one is present).
//!
//! Two rules the renderer has to respect, both from the app itself:
//! - a card painted below `card::BODY_MIN_ZOOM` is only an outline: no controls, no preview.
//!   Every shot that should show content sets `wait_results` and is checked against that zoom.
//! - the camera fits a world rect, so the other axis shows more than the rect. The composition
//!   check complains when a card the shot frames is cut by the frame edge.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use egui::{Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use nodecraft_types::{AssetRef, TagId};
use nodecraft_ui_egui::app::NodeCraftApp;
use nodecraft_ui_egui::{card, theme::ThemeKind};

use crate::scene::{self, Fail, Key, Stage, fail};

/// What to draw, where to point the camera, and which panels are open.
#[derive(Clone, Copy)]
pub struct Shot {
    pub name: &'static str,
    /// The caption, for the log and the README.
    pub caption: &'static str,
    pub size: (f32, f32),
    pub theme: ThemeKind,
    /// Card keys the camera frames; empty frames every card. Ignored when `frame_rect` is set.
    pub frame: &'static [Key],
    /// An art-directed world rect `(x, y, width, height)` for the camera, instead of card bounds.
    pub frame_rect: Option<(f32, f32, f32, f32)>,
    pub padding: f32,
    /// Screen-space push after fitting, to move content clear of an open panel.
    pub nudge: (f32, f32),
    /// A fixed zoom instead of fitting the frame.
    pub zoom: Option<f32>,
    pub select: &'static [Key],
    pub minimap: bool,
    pub search: Option<&'static str>,
    pub tags: &'static [Key],
    pub comments: bool,
    /// Type this into the quick-add palette after opening it with a double-click.
    pub palette: Option<&'static str>,
    /// An entry that must actually be visible after typing, not merely a guessed popup state.
    pub palette_entry: Option<&'static str>,
    pub palette_at: (f32, f32),
    /// Cards whose live result must have landed before the frame is captured.
    pub wait_results: &'static [Key],
}

impl Default for Shot {
    fn default() -> Self {
        Self {
            name: "",
            caption: "",
            size: (1_600.0, 900.0),
            theme: ThemeKind::Dark,
            frame: &[],
            frame_rect: None,
            padding: 40.0,
            nudge: (0.0, 0.0),
            zoom: None,
            select: &[],
            minimap: true,
            search: None,
            tags: &[],
            comments: false,
            palette: None,
            palette_entry: None,
            palette_at: (0.0, 0.0),
            wait_results: &[],
        }
    }
}

const CHAIN: &[Key] = &[5, 6, 7, 8, 17, 18, 16, 21, 19, 20, 22, 23];
const MODALITY: &[Key] = &[12, 13, 14, 15, 10, 11];
/// Three coloured image-workflow rows: `x, y, width, height` in world points.
const CHAIN_RECT: (f32, f32, f32, f32) = (-1_170.0, -100.0, 2_310.0, 2_920.0);
/// Cards the live pass recomputes; a shot that shows them waits for their results.
const LIVE: &[Key] = &[6, 7, 8, 16, 18, 20, 21, 22, 23];

/// Every shot in the gallery, in README order.
pub fn shots() -> Vec<Shot> {
    vec![
        Shot {
            name: "hero",
            caption: "Three original plates, parallel local looks and two recombinations, organised in coloured fields.",
            size: (2_200.0, 2_350.0),
            frame: CHAIN,
            frame_rect: Some(CHAIN_RECT),
            select: &[],
            wait_results: LIVE,
            minimap: false,
            ..Default::default()
        },
        Shot {
            name: "detail-grade",
            caption: "One card: a decoded preview and four sliders the schema generated.",
            size: (1_200.0, 1_150.0),
            frame: &[6],
            zoom: Some(1.35),
            minimap: false,
            wait_results: &[6],
            ..Default::default()
        },
        Shot {
            name: "detail-chain",
            caption: "The image chain: each card shows the result the live pass actually computed.",
            size: (1_900.0, 820.0),
            padding: 110.0,
            frame: &[5, 6, 7, 8],
            minimap: false,
            wait_results: &[6, 7, 8],
            ..Default::default()
        },
        Shot {
            name: "detail-fields",
            caption: "A coloured field contains the aurora tint, the parallel noir look and their two-input overlay.",
            size: (2_000.0, 1_050.0),
            frame: &[17, 18, 16, 21],
            frame_rect: Some((-1_170.0, 850.0, 2_310.0, 1_020.0)),
            minimap: false,
            wait_results: &[16, 18, 21],
            ..Default::default()
        },
        Shot {
            name: "detail-composite",
            caption: "Mirror an original bloom, overlay it onto the look board, then resize the real computed result.",
            size: (2_000.0, 1_050.0),
            frame: &[19, 20, 22, 23],
            frame_rect: Some((-1_170.0, 1_770.0, 2_310.0, 1_020.0)),
            minimap: false,
            wait_results: &[20, 22, 23],
            ..Default::default()
        },
        Shot {
            name: "detail-ports",
            caption: "Port rings, output handles, and numbered order badges on a list input.",
            size: (860.0, 780.0),
            frame: &[4],
            padding: 70.0,
            minimap: false,
            ..Default::default()
        },
        Shot {
            name: "palette",
            caption: "Quick-add: double-click empty canvas, type, create with one key.",
            size: (2_000.0, 1_150.0),
            frame: &[5, 6, 7, 8],
            frame_rect: Some((-1_250.0, -100.0, 2_600.0, 1_150.0)),
            minimap: false,
            palette: Some("grade"),
            palette_entry: Some("Color grade"),
            palette_at: (-150.0, -80.0),
            wait_results: &[6, 7, 8],
            ..Default::default()
        },
        Shot {
            name: "tags",
            caption: "Named colour tags, and the filter that highlights matching cards.",
            size: (2_000.0, 1_150.0),
            frame: &[5, 6, 7, 8],
            frame_rect: Some((-1_250.0, -100.0, 2_600.0, 1_150.0)),
            minimap: false,
            nudge: (150.0, 0.0),
            tags: &[1],
            wait_results: &[6, 7, 8],
            ..Default::default()
        },
        Shot {
            name: "comments",
            caption: "Discussions pinned to a card, with replies, on the canvas itself.",
            size: (1_700.0, 900.0),
            frame: &[6],
            padding: 60.0,
            nudge: (200.0, 0.0),
            minimap: false,
            comments: true,
            wait_results: &[6],
            ..Default::default()
        },
        Shot {
            name: "modality",
            caption: "Typed text and image cables through routers, with real local sources rather than empty media placeholders.",
            size: (1_600.0, 850.0),
            padding: 24.0,
            frame: MODALITY,
            // wider than the cards on purpose: the zoom bar stays in the margin below them
            frame_rect: Some((2_100.0, -360.0, 1_900.0, 1_100.0)),
            minimap: false,
            ..Default::default()
        },
        Shot {
            name: "notes",
            caption: "The brief travels with the work: a note card and the prompt it grew from.",
            size: (780.0, 860.0),
            frame: &[9, 1, 2],
            padding: 70.0,
            minimap: false,
            ..Default::default()
        },
        Shot {
            name: "chrome",
            caption: "The shell around the canvas: dot grid, top bar, minimap and zoom badge.",
            size: (2_000.0, 1_100.0),
            frame: &[5, 6, 7, 8],
            frame_rect: Some((-1_200.0, -100.0, 2_700.0, 1_200.0)),
            wait_results: &[6, 7, 8],
            ..Default::default()
        },
        Shot {
            name: "light",
            caption: "The same document in the light theme: one set of tokens, both palettes.",
            size: (2_200.0, 2_350.0),
            frame: CHAIN,
            frame_rect: Some(CHAIN_RECT),
            theme: ThemeKind::Light,
            wait_results: LIVE,
            minimap: false,
            ..Default::default()
        },
    ]
}

/// The union of the framed cards' world rects, plus room for the label above and the handles
/// beside a card. `None` if a key is not in the document.
fn card_bounds(app: &NodeCraftApp, keys: &[Key]) -> Option<Rect> {
    let canvas = app.session.document().canvases().get(&app.canvas)?;
    let keys: Vec<Key> = if keys.is_empty() { canvas.nodes().keys().filter_map(|id| id.as_bytes().first().copied()).collect() } else { keys.to_vec() };
    let mut bounds: Option<Rect> = None;
    for key in keys {
        let rect = card_bounds_rect(app, key)?;
        bounds = Some(bounds.map_or(rect, |some| some.union(rect)));
    }
    bounds
}

/// One card's world rect, label and handles included.
fn card_bounds_rect(app: &NodeCraftApp, key: Key) -> Option<Rect> {
    let canvas = app.session.document().canvases().get(&app.canvas)?;
    let node = canvas.nodes().get(&scene::node_id(key))?;
    let descriptor = app.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor());
    Some(card::world_rect(node, descriptor, app.theme.tokens()).expand2(Vec2::new(30.0, 46.0)))
}

/// The same rect, without the framing allowance: what a panel would cover.
/// How far a card reaches beyond its body: the title label above it and the port dots beside it.
/// Both the composition check and the camera's overlay avoidance use it, so they agree.
const CARD_MARGIN: Vec2 = Vec2::new(26.0, 42.0);

fn card_rect(app: &NodeCraftApp, key: Key) -> Option<Rect> {
    let canvas = app.session.document().canvases().get(&app.canvas)?;
    let node = canvas.nodes().get(&scene::node_id(key))?;
    let descriptor = app.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor());
    Some(card::world_rect(node, descriptor, app.theme.tokens()))
}

/// Composition complaints, in screen space: a card the frame edge cuts, a framed card under the
/// navigation overlay, and a camera so wide the app would paint card outlines only. Context cards cropped by
/// the edge are only counted, because a closeup is meant to show its neighbours cut off.
fn check_composition(app: &NodeCraftApp, shot: &Shot) -> Result<(), Fail> {
    let canvas = app.session.document().canvases().get(&app.canvas).ok_or_else(|| fail("showcase canvas is missing"))?;
    let tokens = app.theme.tokens();
    let viewport = app.viewport;
    let overlay = nodecraft_ui_egui::navigation::layout(viewport, shot.minimap, tokens).overlay;
    let mut cut: Vec<&str> = Vec::new();
    let mut covered: Vec<&str> = Vec::new();
    let mut context = 0usize;
    for node in canvas.nodes().values() {
        let descriptor = app.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor());
        let world = card::world_rect(node, descriptor, tokens).expand2(CARD_MARGIN);
        let screen = Rect::from_min_max(app.view.to_screen(viewport.min, world.min), app.view.to_screen(viewport.min, world.max));
        let framed = shot.frame.is_empty() || shot.frame.iter().any(|key| scene::node_id(*key) == node.id);
        if !framed && !screen.intersects(viewport) {
            continue;
        }
        let whole = viewport.contains(screen.min) && viewport.contains(screen.max);
        match (framed, whole) {
            (true, false) => cut.push(node.title.as_str()),
            (false, false) => context += 1,
            (true, true) => {
                if screen.intersects(overlay) {
                    covered.push(node.title.as_str());
                }
            }
            (false, true) => {}
        }
    }
    if !cut.is_empty() {
        return Err(fail(format!("{}: cut by the frame edge: {}", shot.name, cut.join(", "))));
    }
    if !covered.is_empty() {
        return Err(fail(format!("{}: under the navigation overlay: {}", shot.name, covered.join(", "))));
    }
    if context > 0 {
        eprintln!("{}: {} context card(s) cropped by the edge", shot.name, context);
    }
    if app.view.zoom <= card::BODY_MIN_ZOOM {
        return Err(fail(format!("{}: zoom {:.0}% must exceed BODY_MIN_ZOOM", shot.name, app.view.zoom * 100.0)));
    }
    Ok(())
}

/// The navigation panel is an overlay painted over the canvas, not beside it - the map above the
/// zoom bar, or just the zoom bar - so pan the framed cards out from under it by the smallest
/// amount that clears it, using the app's own overlay geometry.
fn clear_of_minimap(harness: &mut Harness<'static, NodeCraftApp>, shot: &Shot) {
    let viewport = harness.state().viewport;
    let overlay = nodecraft_ui_egui::navigation::layout(viewport, shot.minimap, harness.state().theme.tokens()).overlay;
    let mut union: Option<Rect> = None;
    for key in shot.frame {
        let Some(rect) = card_rect(harness.state(), *key).map(|some| some.expand2(CARD_MARGIN)) else { continue };
        let state = harness.state();
        let screen = Rect::from_min_max(state.view.to_screen(viewport.min, rect.min), state.view.to_screen(viewport.min, rect.max));
        union = Some(union.map_or(screen, |some| some.union(screen)));
    }
    let Some(blocked) = union.filter(|some| some.intersects(overlay)) else { return };
    let shift_left = overlay.min.x - blocked.max.x - 16.0;
    let shift_up = overlay.min.y - blocked.max.y - 16.0;
    let delta = if shift_left.abs() <= shift_up.abs() { Vec2::new(shift_left, 0.0) } else { Vec2::new(0.0, shift_up) };
    harness.state_mut().view.pan_by(delta);
}

/// Double-click empty canvas, the gesture that opens the quick-add palette.
fn double_click(harness: &mut Harness<'static, NodeCraftApp>, at: Pos2) {
    harness.hover_at(at);
    harness.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            harness.step();
        }
    }
}

/// Wait for real live results before fitting, while the camera may still be elsewhere.
fn settle_results(harness: &mut Harness<'static, NodeCraftApp>, wait_for: &[Key]) -> Result<(), Fail> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        harness.run_ok();
        if wait_for.iter().all(|key| scene::has_result(harness.state(), *key)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let missing: Vec<String> = wait_for.iter().filter(|key| !scene::has_result(harness.state(), **key)).map(|key| key.to_string()).collect();
    Err(fail(format!("no live result on required card key(s) {}; refusing to write empty previews", missing.join(", "))))
}

/// Every image identity the shot promises, including imported sources. Sizes are not identities:
/// dawn and several actions share 896×504, while aurora/bloom and the resized output do not.
fn expected_previews(app: &NodeCraftApp, shot: &Shot) -> Result<Vec<(Key, AssetRef)>, Fail> {
    let canvas = app.session.document().canvases().get(&app.canvas).ok_or_else(|| fail("showcase canvas is missing"))?;
    let mut expected = Vec::new();
    for key in shot.frame {
        let node = canvas.nodes().get(&scene::node_id(*key)).ok_or_else(|| fail(format!("missing framed card {key}")))?;
        match nodecraft_engine::split::image_of(node) {
            Some(asset) => expected.push((*key, asset.clone())),
            None if node.kind == "source.image" || node.kind.starts_with("image.") => return Err(fail(format!("framed image card {key} has no real image"))),
            None => {}
        }
    }
    Ok(expected)
}

/// Each expected asset must have a decoded-image widget inside its own projected card. The
/// product emits this label only for Preview::Ready. Dimensions alone are not an identity: the
/// containment check prevents another same-sized card, including a source, from standing in.
fn framed_previews_pending(harness: &Harness<'static, NodeCraftApp>, shot: &Shot) -> Result<Vec<Key>, Fail> {
    let expected = expected_previews(harness.state(), shot)?;
    let state = harness.state();
    let mut pending = Vec::new();
    for (key, asset) in expected {
        let (width, height) = asset.meta.width.zip(asset.meta.height).ok_or_else(|| fail(format!("framed card {key} image has no dimensions")))?;
        let world = card_rect(state, key).ok_or_else(|| fail(format!("missing framed card {key}")))?;
        let screen = state.view.rect_to_screen(state.viewport.min, world);
        let label = format!("Image {width}×{height}");
        let ppp = harness.ctx.pixels_per_point();
        if !harness.query_all_by_label(&label).any(|widget| {
            // Node::rect() asserts bounds exist. Missing accessibility bounds must instead keep
            // this preview pending, never turn a failed capture into another tool panic.
            widget.accesskit_node().bounding_box().is_some_and(|bounds| {
                let rect =
                    Rect::from_min_max(Pos2::new(bounds.x0 as f32 / ppp, bounds.y0 as f32 / ppp), Pos2::new(bounds.x1 as f32 / ppp, bounds.y1 as f32 / ppp));
                screen.contains_rect(rect)
            })
        }) {
            pending.push(key);
        }
    }
    Ok(pending)
}

/// A field-header double-click collapses that field, not opens quick-add. Verify the actual
/// palette input and result row rather than treating a successful pointer gesture as evidence.
fn verify_palette_open(harness: &Harness<'static, NodeCraftApp>, shot: &Shot) -> Result<(), Fail> {
    let Some(query) = shot.palette else { return Ok(()) };
    let entry = shot.palette_entry.ok_or_else(|| fail("palette shot needs an expected entry"))?;
    let input = harness.query_all_by_role(egui::accesskit::Role::TextInput).any(|widget| {
        let node = widget.accesskit_node();
        node.is_focused() && node.value().as_deref() == Some(query)
    });
    let row = harness.query_all_by_label_contains(entry).any(|widget| widget.accesskit_node().role() == egui::accesskit::Role::Button);
    if !input || !row {
        return Err(fail(format!("quick-add did not open with query {query:?} and entry {entry:?}")));
    }
    Ok(())
}

fn settle_previews(harness: &mut Harness<'static, NodeCraftApp>, shot: &Shot) -> Result<(), Fail> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        harness.run_ok();
        let pending = framed_previews_pending(harness, shot)?;
        if pending.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(fail(format!("decoded previews did not arrive for framed card key(s) {pending:?}; refusing to write placeholders")));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Render one shot and write it to `out/<name>.png`.
///
/// # Errors
/// Returns a message if the document, the renderer or the file write fails.
pub fn render(shot: &Shot, stage: Stage, scale: f32, out: &Path, max_width: u32) -> Result<String, Fail> {
    std::fs::create_dir_all(out).map_err(|error| fail(format!("create {}: {error}", out.display())))?;
    let mut harness = Harness::builder()
        .with_size(Vec2::new(shot.size.0, shot.size.1))
        .with_pixels_per_point(scale)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(600)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), stage.app);
    nodecraft_ui_egui::theme::apply(&harness.ctx, shot.theme);
    // Frame one centres the document at 100%; the camera only moves after that.
    harness.run_ok();

    {
        let state = harness.state_mut();
        state.theme = shot.theme;
        state.selection.clear();
        for key in shot.select {
            state.selection.add(scene::node_id(*key));
        }
        state.navigation.visible = shot.minimap;
        state.search.open = shot.search.is_some();
        if let Some(query) = shot.search {
            state.search.set_query(query).map_err(|error| fail(error.message))?;
        }
        state.tag_filter.open = !shot.tags.is_empty();
        if !shot.tags.is_empty() {
            let tags: BTreeSet<TagId> = shot.tags.iter().map(|key| scene::tag_id(*key)).collect();
            state.tag_filter.set(tags, false);
        }
        state.comments.open = shot.comments;
        if shot.comments {
            state.comments.active = Some(scene::comment_id(1));
        }
    }

    // Live results add a preview band and history strip to a card. Fit only after those bounds
    // have settled; fitting the empty action cards is how a valid-looking camera clips results.
    settle_results(&mut harness, shot.wait_results)?;

    {
        let viewport = harness.state().viewport;
        let bounds = match shot.frame_rect {
            Some((x, y, width, height)) => Rect::from_min_size(Pos2::new(x, y), Vec2::new(width, height)),
            None => card_bounds(harness.state(), shot.frame).ok_or_else(|| fail("framed cards are missing from the document"))?,
        };
        let state = harness.state_mut();
        match shot.zoom {
            Some(zoom) => {
                state.view.zoom = zoom;
                state.view.center_on(viewport, f64::from(bounds.center().x), f64::from(bounds.center().y));
            }
            None => match state.view.fit(viewport, bounds, shot.padding) {
                None => eprintln!("{}: the camera could not fit the frame; the shot is mis-framed", shot.name),
                Some(fit) if fit.clamped => eprintln!("{}: the fit was clamped to the zoom range", shot.name),
                Some(_) => {}
            },
        }
        state.view.pan_by(Vec2::new(shot.nudge.0, shot.nudge.1));
    }
    harness.run_ok();
    clear_of_minimap(&mut harness, shot);
    harness.run_ok();
    check_composition(harness.state(), shot)?;
    settle_previews(&mut harness, shot)?;

    if shot.palette.is_some() {
        let at = {
            let state = harness.state();
            state.view.to_screen(state.viewport.min, Pos2::new(shot.palette_at.0, shot.palette_at.1))
        };
        double_click(&mut harness, at);
        if let Some(query) = shot.palette {
            for letter in query.chars() {
                harness.event(egui::Event::Text(letter.to_string()));
                harness.step();
            }
        }
    }

    for _ in 0..30 {
        harness.run_ok();
        std::thread::sleep(Duration::from_millis(4));
    }
    harness.remove_cursor();
    harness.step();
    // Recheck the exact frame being written, including any late text-action history growth.
    check_composition(harness.state(), shot)?;
    verify_palette_open(&harness, shot)?;
    let pending = framed_previews_pending(&harness, shot)?;
    if !pending.is_empty() {
        return Err(fail(format!("framed previews changed before capture: {pending:?}")));
    }
    let frame = harness.render().map_err(fail)?;
    let (width, height) = (frame.width(), frame.height());
    let image = image::DynamicImage::ImageRgba8(frame);
    let image = if width > max_width { image.resize(max_width, u32::MAX, image::imageops::FilterType::Lanczos3) } else { image };
    let path = out.join(format!("{}.png", shot.name));
    image.save(&path).map_err(|error| fail(format!("write {}: {error}", path.display())))?;
    let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    Ok(format!("{:<14} {:>5}x{:<5} {:>5.0}% -> {:>7.0} KiB", shot.name, width, height, harness.state().view.zoom * 100.0, bytes as f64 / 1024.0))
}

#[cfg(test)]
mod composition_tests {
    use super::*;

    #[test]
    fn one_ready_preview_cannot_satisfy_a_different_same_size_image() -> Result<(), Fail> {
        let mut stage = Stage::new()?;
        for ((key, x), (name, bytes)) in [(17, 100.0), (19, 500.0)].into_iter().zip(crate::art::plates()?.into_iter().skip(1)) {
            stage.import_image(key, name, &bytes, x, 100.0)?;
        }
        stage.app.viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_200.0, 900.0));
        stage.app.view.zoom = 1.0;
        stage.app.view.center_on(stage.app.viewport, 600.0, 450.0);
        stage.app.search.open = false;
        let mut harness = Harness::builder().with_size(Vec2::new(1_200.0, 900.0)).build_ui_state(
            |ui, app: &mut NodeCraftApp| {
                ui.put(Rect::from_min_size(Pos2::new(120.0, 120.0), Vec2::splat(100.0)), egui::Label::new("Image 512×512"));
                if app.search.open {
                    ui.put(Rect::from_min_size(Pos2::new(520.0, 120.0), Vec2::splat(100.0)), egui::Label::new("Image 512×512"));
                }
            },
            stage.app,
        );
        harness.run_ok();
        let shot = Shot { frame: &[17, 19], ..Default::default() };
        let expected = expected_previews(harness.state(), &shot)?;
        assert_eq!(expected.len(), 2);
        assert_eq!(harness.query_all_by_label("Image 512×512").count(), 1);
        assert_eq!(framed_previews_pending(&harness, &shot)?, vec![19], "aurora's ready widget cannot substitute for bloom's missing one");
        harness.state_mut().search.open = true;
        harness.run_ok();
        assert_eq!(harness.query_all_by_label("Image 512×512").count(), 2);
        assert!(framed_previews_pending(&harness, &shot)?.is_empty());
        let palette = Shot { palette: Some("grade"), palette_entry: Some("Color grade"), ..shot };
        assert!(verify_palette_open(&harness, &palette).is_err(), "ready images alone do not prove that quick-add opened");
        harness.state_mut().search.open = false;
        harness.run_ok();
        assert_eq!(framed_previews_pending(&harness, &shot)?, vec![19], "a disappearing preview, such as a collapsed field, must stay an error");
        Ok(())
    }

    #[test]
    fn quick_add_verification_requires_the_actual_query_and_result() -> Result<(), Fail> {
        let app = Stage::new()?.app;
        let mut palette = nodecraft_ui_egui::palette::Palette::new(Pos2::ZERO, Pos2::new(100.0, 100.0));
        palette.query = "grade".to_owned();
        let entries = vec![nodecraft_ui_egui::palette::Entry {
            target: nodecraft_ui_egui::palette::Target::Kind("image.grade".to_owned()),
            label: "Color grade".to_owned(),
            shortcut: None,
        }];
        let mut harness = Harness::builder().build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                let _ = palette.show(ui.ctx(), &entries, app.theme.tokens());
            },
            app,
        );
        harness.run_ok();
        let shot = Shot { palette: Some("grade"), palette_entry: Some("Color grade"), ..Default::default() };
        verify_palette_open(&harness, &shot)?;
        assert!(verify_palette_open(&harness, &Shot { palette: Some("blur"), ..shot }).is_err());
        assert!(verify_palette_open(&harness, &Shot { palette_entry: Some("Blur"), ..shot }).is_err());
        Ok(())
    }

    #[test]
    fn expected_previews_include_all_framed_sources_not_just_live_actions() -> Result<(), Fail> {
        let stage = scene::showcase(&crate::art::plates()?)?;
        let shot = Shot { frame: &[5, 17, 19], ..Default::default() };
        assert_eq!(expected_previews(&stage.app, &shot)?.iter().map(|(key, _)| *key).collect::<Vec<_>>(), vec![5, 17, 19]);
        assert!(expected_previews(&stage.app, &Shot { frame: &[23], ..Default::default() }).is_err(), "an uncomputed resize is not preview-ready");
        Ok(())
    }

    #[test]
    fn a_framed_card_entirely_outside_the_camera_is_an_error() -> Result<(), Fail> {
        let mut stage = Stage::new()?;
        stage.node(1, "source.text", 10_000.0, 10_000.0)?;
        stage.app.viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        stage.app.view.zoom = 1.0;
        stage.app.view.center_on(stage.app.viewport, 0.0, 0.0);
        let shot = Shot { name: "offscreen-regression", frame: &[1], minimap: false, ..Default::default() };
        assert!(check_composition(&stage.app, &shot).is_err());
        Ok(())
    }
}
