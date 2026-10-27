//! Compatible card-body wiring, through actual pointer events (#951).
use super::*;
use egui_kittest::Harness;

const SOURCE: NodeId = NodeId::from_bytes([141; 16]);
const TARGET: NodeId = NodeId::from_bytes([142; 16]);

fn harness(render: bool, zoom: f32) -> Harness<'static, NodeCraftApp> {
    harness_for(render, zoom, "structure.router")
}

fn harness_for(render: bool, zoom: f32, target_kind: &str) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let existing: Vec<_> = app.session.document().canvases().get(&canvas).unwrap().nodes().keys().copied().collect();
    for id in existing {
        app.session.dispatch("node.remove", &json!({"canvas":canvas,"id":id})).unwrap();
    }
    for (id, kind, x) in [(SOURCE, "source.text", -400), (TARGET, target_kind, 60)] {
        app.session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":kind,"pos":{"x":x,"y":0}})).unwrap();
    }
    app.centred = true;
    app.view = CanvasView { pan: Vec2::new(520.0, 350.0), zoom };
    let builder = Harness::builder().with_size(Vec2::new(1400.0, 1000.0)).with_max_steps(160);
    let builder = if render { builder.wgpu() } else { builder };
    let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    theme::apply(&h.ctx, ThemeKind::Dark);
    h.run();
    h
}

fn source(h: &Harness<'static, NodeCraftApp>) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&SOURCE).unwrap();
    let rect = app.painted.get(&SOURCE).copied().unwrap();
    let at = noodle::port_anchor(node, rect, Side::Output, &node.outputs.first().unwrap().name).unwrap();
    app.view.to_screen(app.viewport.min, at)
}

fn body(h: &Harness<'static, NodeCraftApp>) -> Pos2 {
    body_of(h, TARGET)
}

fn body_of(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> Pos2 {
    let app = h.state();
    let rect = app.view.rect_to_screen(app.viewport.min, *app.painted.get(&id).unwrap());
    // Clear of the input's existing screen-space reach, even at overview zoom.
    let visible = rect.intersect(app.viewport.shrink(app.theme.tokens().navigation_margin));
    Pos2::new(rect.left() + (rect.width() * 0.8).min(60.0), visible.center().y)
}

fn point(h: &Harness<'static, NodeCraftApp>, id: NodeId, side: Side, name: &str) -> Pos2 {
    let app = h.state();
    let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
    let rect = app.painted.get(&id).copied().unwrap();
    app.view.to_screen(app.viewport.min, noodle::port_anchor(node, rect, side, name).unwrap())
}

fn highlighted(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> bool {
    let app = h.state();
    let t = app.theme.tokens();
    let expected = app.view.rect_to_screen(app.viewport.min, *app.painted.get(&id).unwrap()).expand(t.drop_ring_gap);
    h.output().shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::epaint::Shape::Rect(rect)
        if rect.stroke.color == t.accent && rect.stroke.width == t.drop_ring_width
            && rect.rect.min.distance(expected.min) < 0.01 && rect.rect.max.distance(expected.max) < 0.01)
    })
}

fn pair_harness() -> Harness<'static, NodeCraftApp> {
    super::tests::chain_harness_with(|mut descriptor| {
        if descriptor.id == "test.relay" {
            let mut spare = descriptor.inputs.first().unwrap().clone();
            spare.name = "spare".into();
            descriptor.inputs.push(spare);
        }
        Box::new(super::tests::TestKind(descriptor))
    })
}

fn connect(h: &mut Harness<'static, NodeCraftApp>, from: NodeId, to: NodeId, port: &str, id: u8, order: u32) {
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch(
            "edge.connect",
            &json!({"canvas":canvas,"edge":{
                "id":EdgeId::from_bytes([id;16]),"from":{"node":from,"port":"out"},"to":{"node":to,"port":port},"order":order
            }}),
        )
        .unwrap();
    h.run();
}

fn extra_source(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) {
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":"source.text","pos":{"x":-2000,"y":0}})).unwrap();
    h.run();
}

fn release(h: &mut Harness<'static, NodeCraftApp>, to: Pos2) {
    h.drop_at(to);
    h.run();
}

fn hover_wire(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run();
    h.drag_at(from);
    h.run();
    h.hover_at(from + (to - from) * 0.5);
    h.run();
    h.hover_at(to);
    h.run();
    assert!(h.state().wiring.is_some());
}

#[test]
fn a_body_drop_connects_its_compatible_input_in_one_undo_at_every_zoom() {
    for zoom in [0.1, 0.5, 1.0, 4.0] {
        let mut h = harness(false, zoom);
        let before = h.state().session.document().clone();
        let (from, to) = (source(&h), body(&h));
        hover_wire(&mut h, from, to);
        h.drop_at(to);
        h.run();
        let edges = h.state().session.document().canvases().get(&h.state().canvas).unwrap().edges();
        assert_eq!(edges.len(), 1, "body drop must connect at zoom {zoom}; notice={:?}", h.state().notice);
        let edge = edges.values().next().unwrap();
        assert_eq!((edge.from.node, edge.to.node), (SOURCE, TARGET));
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document(), &before, "one undo restores the graph at zoom {zoom}");
    }
}

#[test]
fn compatible_hover_uses_the_theme_outline_at_every_zoom_without_editing() {
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        for zoom in [0.1, 0.5, 1.0, 4.0] {
            let mut h = harness(false, zoom);
            h.state_mut().theme = kind;
            theme::apply(&h.ctx, kind);
            h.run();
            let before = h.state().session.document().clone();
            let (from, to) = (source(&h), body(&h));
            assert!(!highlighted(&h, TARGET));
            hover_wire(&mut h, from, to);
            assert!(highlighted(&h, TARGET), "{kind:?}, zoom {zoom}");
            assert_eq!(h.state().session.document(), &before);
            h.key_press(egui::Key::Escape);
            h.run();
            release(&mut h, to);
            assert!(!highlighted(&h, TARGET));
            assert_eq!(h.state().session.document(), &before);
        }
    }
}

#[test]
fn body_prefers_the_first_free_input_while_precise_drop_still_replaces_the_chosen_slot() {
    for precise in [false, true] {
        let mut h = pair_harness();
        let target = super::tests::chain_id(2);
        let input = point(&h, target, Side::Input, "in");
        let to = if precise { input } else { Pos2::new(body_of(&h, target).x, input.y) };
        let from = point(&h, super::tests::chain_id(4), Side::Output, "out");
        let before = h.state().session.document().clone();
        hover_wire(&mut h, from, to);
        release(&mut h, to);
        let port = if precise { "in" } else { "spare" };
        assert!(
            h.state()
                .session
                .document()
                .canvases()
                .get(&h.state().canvas)
                .unwrap()
                .edges()
                .values()
                .any(|edge| edge.from.node == super::tests::chain_id(4) && edge.to.node == target && edge.to.port == port)
        );
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
fn all_occupied_inputs_choose_the_nearest_and_replace_atomically() {
    let mut h = pair_harness();
    let target = super::tests::chain_id(2);
    let other = super::tests::chain_id(5);
    extra_source(&mut h, other);
    connect(&mut h, other, target, "spare", 55, 0);
    let before = h.state().session.document().clone();
    let slot = point(&h, target, Side::Input, "spare");
    let to = Pos2::new(body_of(&h, target).x, slot.y);
    let from = point(&h, super::tests::chain_id(4), Side::Output, "out");
    hover_wire(&mut h, from, to);
    release(&mut h, to);
    let edges = h.state().session.document().canvases().get(&h.state().canvas).unwrap().edges();
    assert!(edges.values().any(|edge| edge.from.node == super::tests::chain_id(4) && edge.to.node == target && edge.to.port == "spare"));
    assert!(edges.values().any(|edge| edge.from.node == super::tests::chain_id(1) && edge.to.node == target && edge.to.port == "in"));
    assert!(!edges.values().any(|edge| edge.from.node == other));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn body_appends_checked_list_order_and_refuses_an_exhausted_list() {
    for order in [7, u32::MAX] {
        let mut h = harness(false, 1.0);
        let other = NodeId::from_bytes([143; 16]);
        extra_source(&mut h, other);
        connect(&mut h, other, TARGET, "in", 151, order);
        let before = h.state().session.document().clone();
        let (from, to) = (source(&h), body(&h));
        hover_wire(&mut h, from, to);
        assert_eq!(highlighted(&h, TARGET), order != u32::MAX);
        release(&mut h, to);
        if order == u32::MAX {
            assert_eq!(h.state().session.document(), &before);
        } else {
            let edge = h.state().session.document().canvases().get(&h.state().canvas).unwrap().edges().values().find(|edge| edge.from.node == SOURCE).unwrap();
            assert_eq!(edge.order, 8);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &before);
        }
    }
}

#[test]
fn incompatible_body_or_front_card_does_not_highlight_or_accept_a_wire() {
    for mode in 0..3 {
        let front = mode != 0;
        let mut h = harness_for(false, 1.0, if front { "structure.router" } else { "image.blur" });
        let front_id = NodeId::from_bytes([if mode == 2 { 140 } else { 143 }; 16]);
        if front {
            let canvas = h.state().canvas;
            h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":front_id,"kind":"source.image","pos":{"x":60,"y":0}})).unwrap();
            h.run();
        }
        let before = h.state().session.document().clone();
        let (from, to) = (source(&h), body(&h));
        hover_wire(&mut h, from, to);
        if mode == 2 {
            // A selected lower-id card paints in front of the higher-id Router too.
            h.state_mut().selection.only(front_id);
            h.run();
        }
        assert!(!highlighted(&h, TARGET));
        if front {
            assert!(!highlighted(&h, front_id));
        }
        release(&mut h, to);
        assert_eq!(h.state().session.document(), &before);
        assert!(h.state().turn_into.is_none());
    }
}

#[test]
fn a_picked_up_wire_survives_refused_body_and_minimap_drops() {
    for overlay in [false, true] {
        let mut h = harness(false, 1.0);
        connect(&mut h, SOURCE, TARGET, "in", 152, 0);
        let canvas = h.state().canvas;
        let incompatible = NodeId::from_bytes([143; 16]);
        h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":incompatible,"kind":"image.blur","pos":{"x":60,"y":220}})).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let view = h.state().view;
        let from = point(&h, TARGET, Side::Input, "in");
        let to = if overlay { crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map.center() } else { body_of(&h, incompatible) };
        hover_wire(&mut h, from, to);
        assert!(h.state().wiring.as_ref().unwrap().moving.is_some());
        assert!(!highlighted(&h, incompatible));
        release(&mut h, to);
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().view, view);
    }
}

#[test]
fn self_cycle_and_duplicate_body_drops_preserve_the_graph() {
    for cycle in [false, true] {
        let mut h = super::tests::chain_harness();
        let from_id = super::tests::chain_id(if cycle { 3 } else { 2 });
        let to_id = super::tests::chain_id(2);
        let before = h.state().session.document().clone();
        let (from, to) = (point(&h, from_id, Side::Output, "out"), body_of(&h, to_id));
        hover_wire(&mut h, from, to);
        assert!(!highlighted(&h, to_id));
        release(&mut h, to);
        assert_eq!(h.state().session.document(), &before);
    }
    let mut h = harness(false, 1.0);
    connect(&mut h, SOURCE, TARGET, "in", 153, 0);
    let before = h.state().session.document().clone();
    let (from, to) = (source(&h), body(&h));
    hover_wire(&mut h, from, to);
    assert!(!highlighted(&h, TARGET));
    release(&mut h, to);
    assert_eq!(h.state().session.document(), &before);
}

#[test]
#[ignore = "manual native GPU capture; requires NODECRAFT_WIRING951_RENDER_ROOT"]
fn render_body_wiring() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_WIRING951_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for kind in [ThemeKind::Dark, ThemeKind::Light] {
        let mut h = harness(true, 1.0);
        h.state_mut().theme = kind;
        theme::apply(&h.ctx, kind);
        h.run();
        let (from, to) = (source(&h), body(&h));
        hover_wire(&mut h, from, to);
        h.render().unwrap().save(root.join(format!("{kind:?}-hover.png"))).unwrap();
        h.drop_at(to);
        h.run();
        h.render().unwrap().save(root.join(format!("{kind:?}-released.png"))).unwrap();
    }
}
