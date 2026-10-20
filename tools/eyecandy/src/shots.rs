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
use egui_kittest::kittest::Queryable;
use nodecraft_types::TagId;
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
            palette_at: (0.0, 0.0),
            wait_results: &[],
        }
    }
}

const CHAIN: &[Key] = &[1, 2, 3, 4, 5, 6, 7, 8];
const MODALITY: &[Key] = &[12, 13, 14, 15, 10, 11];
/// The two rows and the note above them: `x, y, width, height` in world points.
const CHAIN_RECT: (f32, f32, f32, f32) = (-1_080.0, -660.0, 2_140.0, 1_460.0);
/// Cards the live pass recomputes; a shot that shows them waits for their results.
const LIVE: &[Key] = &[6, 7, 8];

/// Every shot in the gallery, in README order.
pub fn shots() -> Vec<Shot> {
    vec![
        Shot {
            name: "hero",
            caption: "A dawn look-development pass: the words above, the picture below, one workflow.",
            size: (2_000.0, 1_250.0),
            frame: CHAIN,
            frame_rect: Some(CHAIN_RECT),
            select: &[6, 7],
            wait_results: LIVE,
            minimap: false,
            ..Default::default()
        },
        Shot {
            name: "detail-grade",
            caption: "One card: a decoded preview and four sliders the schema generated.",
            size: (1_200.0, 900.0),
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
            wait_results: LIVE,
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
            size: (1_700.0, 1_050.0),
            frame: CHAIN,
            frame_rect: Some(CHAIN_RECT),
            minimap: false,
            palette: Some("grade"),
            palette_at: (-40.0, -20.0),
            ..Default::default()
        },
        Shot {
            name: "tags",
            caption: "Named colour tags, and the filter that highlights matching cards.",
            size: (1_700.0, 1_050.0),
            frame: CHAIN,
            frame_rect: Some(CHAIN_RECT),
            minimap: false,
            nudge: (150.0, 0.0),
            tags: &[1, 2],
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
            caption: "Six modalities, six wire colours: text, image, video, audio, 3D, document.",
            size: (1_600.0, 620.0),
            padding: 24.0,
            frame: MODALITY,
            // wider than the cards on purpose: the zoom bar stays in the margin below them
            frame_rect: Some((2_100.0, -360.0, 1_900.0, 860.0)),
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
            size: (1_760.0, 1_000.0),
            frame: CHAIN,
            frame_rect: Some((-1_080.0, -680.0, 2_560.0, 1_480.0)),
            wait_results: LIVE,
            ..Default::default()
        },
        Shot {
            name: "light",
            caption: "The same document in the light theme: one set of tokens, both palettes.",
            size: (2_000.0, 1_250.0),
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
fn check_composition(app: &NodeCraftApp, shot: &Shot, _visible: Rect) {
    let Some(canvas) = app.session.document().canvases().get(&app.canvas) else { return };
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
        if !screen.intersects(viewport) {
            continue;
        }
        let whole = viewport.contains(screen.min) && viewport.contains(screen.max);
        let framed = shot.frame.iter().any(|key| scene::node_id(*key) == node.id);
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
        eprintln!("{}: cut by the frame edge: {}", shot.name, cut.join(", "));
    }
    if !covered.is_empty() {
        eprintln!("{}: under the navigation overlay: {}", shot.name, covered.join(", "));
    }
    if context > 0 {
        eprintln!("{}: {} context card(s) cropped by the edge", shot.name, context);
    }
    if app.view.zoom < card::BODY_MIN_ZOOM {
        eprintln!("{}: zoom {:.0}% is below BODY_MIN_ZOOM, so cards are outlines only", shot.name, app.view.zoom * 100.0);
    }
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

/// Run frames until the live pass has recorded a result on every card that needs one, then a few
/// more so the preview worker can decode. Previews and live results both land on worker threads.
fn settle(harness: &mut Harness<'static, NodeCraftApp>, wait_for: &[Key], label: Option<&str>) {
    let mut got_results = wait_for.is_empty();
    let mut preview = label.is_none();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        harness.run_ok();
        if !got_results {
            got_results = wait_for.iter().all(|key| scene::has_result(harness.state(), *key));
        }
        if !preview {
            preview = label.as_ref().is_some_and(|want| harness.query_by_label(want).is_some());
        }
        if got_results && preview {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let missing: Vec<String> = wait_for.iter().filter(|key| !scene::has_result(harness.state(), **key)).map(|key| key.to_string()).collect();
    if !missing.is_empty() {
        eprintln!("note: no live result on card key(s) {} - their previews will read as empty", missing.join(", "));
    }
    if !preview {
        eprintln!("note: the decoded-preview label did not appear in time");
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
    let visible = {
        let state = harness.state();
        state.view.visible_world(state.viewport)
    };
    check_composition(harness.state(), shot, visible);

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

    let want_preview = (!shot.wait_results.is_empty()).then_some("Image 896×504");
    settle(&mut harness, shot.wait_results, want_preview);
    for _ in 0..30 {
        harness.run_ok();
        std::thread::sleep(Duration::from_millis(4));
    }
    harness.remove_cursor();
    harness.step();
    let frame = harness.render().map_err(fail)?;
    let (width, height) = (frame.width(), frame.height());
    let image = image::DynamicImage::ImageRgba8(frame);
    let image = if width > max_width { image.resize(max_width, u32::MAX, image::imageops::FilterType::Lanczos3) } else { image };
    let path = out.join(format!("{}.png", shot.name));
    image.save(&path).map_err(|error| fail(format!("write {}: {error}", path.display())))?;
    let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    Ok(format!("{:<14} {:>5}x{:<5} {:>5.0}% -> {:>7.0} KiB", shot.name, width, height, harness.state().view.zoom * 100.0, bytes as f64 / 1024.0))
}
