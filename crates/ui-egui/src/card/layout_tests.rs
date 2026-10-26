use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::{NodeId, ParamSchema, ParamSpec};

fn descriptor(control: ParamControl) -> NodeDescriptor {
    NodeDescriptor {
        id: "test.layout".into(),
        label: "Layout".into(),
        category: nodecraft_nodes::Category::Source,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema {
            params: vec![ParamSpec {
                name: "value".into(),
                label: "Value".into(),
                description: None,
                control,
                required: false,
                default: Some(Value::Number(Number::new(0.25).unwrap())),
                visible_if: None,
            }],
        },
    }
}
fn slider() -> ParamControl {
    ParamControl::Slider { min: Some(Number::new(0.0).unwrap()), max: Some(Number::new(1.0).unwrap()), step: Number::new(0.01).unwrap() }
}

#[test]
fn compact_metrics_are_bounded_and_reserve_streaming_outputs() {
    let t = &crate::theme::DARK;
    let mut node = Node::new(NodeId::from_bytes([77; 16]), "test.layout");
    let mut d = descriptor(slider());
    let size = world_size(&node, Some(&d), t);
    assert!(size.y < t.card_min_height, "one slider is not a 220-point body: {size:?}");
    assert_eq!(size.x, t.card_width);
    d.params.params = (0..100)
        .map(|i| {
            let mut p = d.params.params[0].clone();
            p.name = format!("p{i}");
            p
        })
        .collect();
    assert_eq!(world_size(&node, Some(&d), t).y, t.card_max_height);
    for kind in [nodecraft_nodes::generation::TEXT_KIND, nodecraft_nodes::generation::AUDIO_KIND] {
        node.kind = kind.into();
        let m = layout::Metrics::of(&node, Some(&d), t);
        assert_eq!(m.preview + m.answer, t.card_preview_height);
        assert_eq!(world_size(&node, Some(&d), t).y, t.card_max_height + t.card_preview_height);
    }
}

#[test]
fn pdf_first_page_and_finished_speech_keep_their_preview_reservations() {
    let t = &crate::theme::DARK;
    let asset =
        |mime: &str| nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([7; 32]), mime: mime.parse().unwrap(), meta: Default::default() };
    let mut pdf = Node::new(NodeId::from_bytes([78; 16]), nodecraft_nodes::document::SOURCE);
    pdf.params.insert("asset".into(), Value::Media(asset("application/pdf")));
    assert_eq!(layout::Metrics::of(&pdf, None, t).preview, t.card_preview_height);

    let mut speech = Node::new(NodeId::from_bytes([79; 16]), nodecraft_nodes::generation::AUDIO_KIND);
    let before = layout::Metrics::of(&speech, None, t);
    let id = nodecraft_types::GenerationId::from_bytes([8; 16]);
    speech.history.push(nodecraft_graph::Generation::new(id, vec![Value::Media(asset("audio/wav"))]));
    speech.active = Some(id);
    let after = layout::Metrics::of(&speech, None, t);
    assert_eq!(before.preview, after.preview, "a finished sound cannot add another preview region");
    assert_eq!(before.parameters, after.parameters);
    assert_eq!(after.size(t).y - before.size(t).y, after.history);
}

#[test]
fn image_action_reserves_one_preview_before_live_drag_and_first_result() {
    let t = &crate::theme::DARK;
    let mut d = descriptor(slider());
    d.category = nodecraft_nodes::Category::Action;
    d.outputs
        .push(nodecraft_graph::Port { name: "image".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: false } });
    let mut node = Node::new(NodeId::from_bytes([80; 16]), "test.layout");
    let before = layout::Metrics::of(&node, Some(&d), t);
    assert_eq!(before.preview, t.card_preview_height);
    let id = nodecraft_types::GenerationId::from_bytes([9; 16]);
    node.history.push(nodecraft_graph::Generation::new(
        id,
        vec![Value::Media(nodecraft_types::AssetRef {
            hash: nodecraft_types::Blake3::from_bytes([7; 32]),
            mime: "image/png".parse().unwrap(),
            meta: Default::default(),
        })],
    ));
    node.active = Some(id);
    let after = layout::Metrics::of(&node, Some(&d), t);
    assert_eq!(before.preview, after.preview);
    assert_eq!(before.parameters, after.parameters);
    assert_eq!(after.size(t).y - before.size(t).y, after.history);
}

struct State {
    node: Node,
    descriptor: NodeDescriptor,
    editor: EditorState,
    intents: Vec<Intent>,
    body: Rect,
    wire_starts: Vec<(Side, String)>,
    preview_inputs: Option<Vec<nodecraft_graph::Port>>,
}
fn harness(zoom: f32, ppp: f32, control: ParamControl, enabled: bool) -> Harness<'static, State> {
    let mut h = Harness::builder().with_max_steps(120).with_size(Vec2::new(1200.0, 1100.0) * ppp).build_ui_state(
        move |ui, state: &mut State| {
            let t = &crate::theme::DARK;
            let view = CanvasView { zoom, pan: Vec2::new(80.0, 80.0) };
            let visible = state.descriptor.params.params.iter().map(|spec| spec.name.clone()).collect::<BTreeSet<_>>();
            let enabled = if enabled { visible.clone() } else { BTreeSet::new() };
            state.body = view.rect_to_screen(ui.max_rect().min, world_rect(&state.node, Some(&state.descriptor), t));
            let card = CardView {
                descriptor: Some(&state.descriptor),
                inputs: state.preview_inputs.as_deref().unwrap_or(&state.descriptor.inputs),
                visible: Some(&visible),
                enabled: Some(&enabled),
                preview_pos: None,
                images: (vec![], 0),
                grid: None,
                roles: None,
                listed: None,
                lists: None,
                prompt_wired: false,
                elements: &BTreeMap::new(),
                mentions: &[],
                mention_blocked: None,
                partial: None,
                local_text: None,
                video: None,
                audio: None,
                mesh: None,
                outdated: false,
                prompt_overlay: true,
                variations: Default::default(),
            };
            if let Some(r) = show(ui, &view, &state.node, card, &mut state.editor, false, t) {
                for (side, name, response) in r.ports {
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        state.wire_starts.push((side, name));
                    }
                }
                for intent in r.intents {
                    if let Intent::Parameter { name, value } = &intent {
                        if let Some(value) = value {
                            state.node.params.insert(name.clone(), value.clone());
                        } else {
                            state.node.params.remove(name);
                        }
                        state.editor.accepted(&EditTarget::Parameter(name.clone()));
                    }
                    state.intents.push(intent);
                }
            }
        },
        State {
            node: Node::new(NodeId::from_bytes([77; 16]), "test.layout"),
            descriptor: descriptor(control),
            editor: EditorState::default(),
            intents: vec![],
            body: Rect::NOTHING,
            wire_starts: vec![],
            preview_inputs: None,
        },
    );
    h.ctx.set_pixels_per_point(ppp);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h
}

fn pointer_click(h: &mut Harness<'static, State>, at: Pos2) {
    h.hover_at(at);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
}

fn port(name: String) -> nodecraft_graph::Port {
    nodecraft_graph::Port { name, port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false } }
}

fn press_port(h: &mut Harness<'static, State>, side: Side, index: usize, count: usize, name: &str) {
    let at = noodle::anchor(h.state().body, side, index, count);
    h.state_mut().wire_starts.clear();
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.hover_at(at + Vec2::new(2.0, 0.0));
    h.run();
    assert_eq!(h.state().wire_starts, vec![(side, name.to_owned())], "wrong wire at {at:?}, body {:?}", h.state().body);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

#[test]
fn each_compact_multiport_anchor_starts_its_own_wire() {
    for zoom in [1.0, 0.3, 0.75] {
        for count in [3, 4] {
            let mut h = harness(zoom, 1.0, slider(), true);
            h.state_mut().descriptor.params.params.clear();
            let inputs = (0..count).map(|i| port(format!("input{i}"))).collect::<Vec<_>>();
            let outputs = (0..count).map(|i| port(format!("output{i}"))).collect::<Vec<_>>();
            h.state_mut().descriptor.inputs = inputs.clone();
            h.state_mut().descriptor.outputs = outputs.clone();
            h.state_mut().node.inputs = inputs;
            h.state_mut().node.outputs = outputs;
            h.run();
            let t = &crate::theme::DARK;
            let pitch = (t.handle_radius.max(t.handle_min_screen_radius) + PORT_HIT) * 2.0 + t.control_gap;
            assert!((h.state().body.height() - (count + 1) as f32 * pitch * zoom).abs() < 0.001);
            for index in 0..count {
                press_port(&mut h, Side::Output, index, count, &format!("output{index}"));
                press_port(&mut h, Side::Input, index, count, &format!("input{index}"));
            }
        }
    }
}

#[test]
fn additive_preview_input_centers_remain_independent_even_without_declared_room() {
    for zoom in [1.0, 0.3, 0.75] {
        let mut h = harness(zoom, 1.0, slider(), true);
        h.state_mut().descriptor.params.params.clear();
        h.state_mut().descriptor.inputs = vec![port("saved".into())];
        h.state_mut().node.inputs = h.state().descriptor.inputs.clone();
        let previews = (0..8).map(|i| port(format!("preview{i}"))).collect::<Vec<_>>();
        h.state_mut().preview_inputs = Some(previews);
        h.run();
        assert!((h.state().body.height() - 64.0 * zoom).abs() < 0.001, "one persisted/declared port keeps its compact body");
        for index in 0..8 {
            press_port(&mut h, Side::Input, index, 8, &format!("preview{index}"));
        }
    }
}

#[test]
fn full_width_slider_and_entry_fit_actual_card_and_drag_commits_on_release() {
    for zoom in [0.75, 1.0, 2.0] {
        for ppp in [1.0, 2.0] {
            let mut h = harness(zoom, ppp, slider(), true);
            let track = h.get_by_role(egui::accesskit::Role::Slider).rect();
            let entry = h.get_by_role(egui::accesskit::Role::TextInput).rect();
            let inner = h.state().body.shrink(crate::theme::DARK.card_padding * zoom);
            for r in [track, entry] {
                assert!((r.left() - inner.left()).abs() < 2.0, "{zoom}/{ppp}: {r:?} {inner:?}");
                assert!((r.right() - inner.right()).abs() < 2.0, "{zoom}/{ppp}: {r:?} {inner:?}");
                assert!(inner.contains_rect(r), "control outside body: {r:?} {inner:?}");
            }
            assert!(inner.bottom() - entry.bottom() < crate::theme::DARK.control_row_height * zoom);
            let start = track.center();
            let end = Pos2::new(track.right() - 4.0 * zoom, track.center().y);
            h.drag_at(start);
            h.run();
            h.hover_at(end);
            h.run();
            assert!(h.state().intents.is_empty(), "dragging is a local preview");
            assert!(h.state().editor.dragging().is_some());
            h.drop_at(end);
            h.run();
            assert_eq!(h.state().intents.len(), 1);
            assert!(matches!(h.state().intents.first(), Some(Intent::Parameter { value: Some(Value::Number(n)), .. }) if n.get() > 0.8));
        }
    }
}

#[test]
fn compact_history_chips_stay_in_their_reserved_region_at_zoom_and_scale() {
    for zoom in [0.75, 1.0, 2.0] {
        let mut h = harness(zoom, 2.0, slider(), true);
        for index in 1..=100_u8 {
            let id = nodecraft_types::GenerationId::from_bytes([index; 16]);
            h.state_mut().node.history.push(nodecraft_graph::Generation::new(id, vec![Value::Text("Take".into())]));
            h.state_mut().node.active = Some(id);
        }
        h.run();
        let chip = h.get_by_label("Result 100 of 100, active").rect();
        let inner = h.state().body.shrink(crate::theme::DARK.card_padding * zoom);
        assert!(inner.contains_rect(chip), "chip {chip:?} outside {inner:?}");
        let entry = h.get_by_role(egui::accesskit::Role::TextInput).rect();
        assert!(entry.bottom() < chip.top(), "history overlaps parameter entry");
    }
}

#[test]
fn four_slider_and_empty_image_bodies_end_at_actual_content() {
    for zoom in [0.75, 1.0, 2.0] {
        let mut h = harness(zoom, 2.0, slider(), true);
        h.state_mut().descriptor.params.params = (0..4)
            .map(|i| {
                let mut p = descriptor(slider()).params.params.remove(0);
                p.name = format!("value{i}");
                p.label = format!("Value {i}");
                p
            })
            .collect();
        h.run();
        let last = h.get_all_by_role(egui::accesskit::Role::TextInput).map(|n| n.rect().bottom()).fold(0.0_f32, f32::max);
        let bottom = h.state().body.bottom() - crate::theme::DARK.card_padding * zoom;
        assert!((bottom - last) / zoom <= 2.0 * crate::theme::DARK.control_gap, "four sliders: blank bottom {} world points", (bottom - last) / zoom);
        assert!(last <= bottom + 1.0, "{zoom}: last slider clipped: last={last}, inner bottom={bottom}, delta={}", (last - bottom) / zoom);

        let mut h = harness(zoom, 2.0, ParamControl::Asset { modality: nodecraft_types::Modality::Image }, true);
        h.state_mut().descriptor.params.params[0].default = None;
        h.run();
        let last = h.get_by_label("No image selected").rect().bottom();
        let bottom = h.state().body.bottom() - crate::theme::DARK.card_padding * zoom;
        assert!((bottom - last) / zoom <= 2.0 * crate::theme::DARK.control_gap, "empty image: blank bottom {} world points", (bottom - last) / zoom);
        assert!(last <= bottom + 1.0, "image content clipped");
    }
}

#[test]
fn exact_slider_entry_defers_typing_and_retains_invalid_drafts() {
    for zoom in [0.75, 2.0] {
        let mut h = harness(zoom, 2.0, slider(), true);
        let at = h.get_by_role(egui::accesskit::Role::TextInput).rect().center();
        pointer_click(&mut h, at);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("0.75".into()));
        h.run();
        assert!(h.state().intents.is_empty());
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(matches!(h.state().intents.as_slice(), [Intent::Parameter { value: Some(Value::Number(n)), .. }] if n.get() == 0.75));

        let mut h = harness(zoom, 2.0, slider(), true);
        let at = h.get_by_role(egui::accesskit::Role::TextInput).rect().center();
        pointer_click(&mut h, at);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("not a number".into()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().intents.is_empty());
        assert_eq!(h.state().editor.text.get("value").unwrap().text, "not a number");
        assert!(h.state().editor.error().is_some());
        let at = h.get_by_role(egui::accesskit::Role::TextInput).rect().center();
        pointer_click(&mut h, at);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().intents.is_empty());
        assert!(h.state().editor.text.is_empty());
    }
}

#[test]
fn choices_and_long_text_previews_stay_inside_compact_bodies() {
    for zoom in [0.75, 2.0] {
        for control in [
            ParamControl::Choice { options: vec![nodecraft_types::Choice { value: "long".into(), label: "A long choice ".repeat(200) }] },
            ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 },
        ] {
            let mut h = harness(zoom, 2.0, control.clone(), true);
            h.state_mut().node.params.insert("value".into(), Value::Text("long".repeat(10000)));
            h.run();
            let role = if matches!(control, ParamControl::Choice { .. }) { egui::accesskit::Role::ComboBox } else { egui::accesskit::Role::TextInput };
            let r = h.get_by_role(role).rect();
            let inner = h.state().body.shrink(crate::theme::DARK.card_padding * zoom);
            assert!((r.width() - inner.width()).abs() < 2.0);
            assert!(inner.contains_rect(r), "{r:?} outside {inner:?}");
        }
    }
}

#[test]
fn long_generation_prompt_editor_keeps_following_controls_reachable_and_identity_stable() {
    for zoom in [0.75, 2.0] {
        let mut h = harness(zoom, 2.0, ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, true);
        let text = "A complete long prompt line\n".repeat(1000);
        h.state_mut().node.kind = nodecraft_nodes::generation::TEXT_KIND.into();
        let mut prompt = h.state().descriptor.params.params[0].clone();
        prompt.name = nodecraft_nodes::generation::PROMPT.into();
        prompt.label = "Prompt".into();
        prompt.default = Some(Value::Text(text.clone()));
        let mut strength = descriptor(slider()).params.params.remove(0);
        strength.name = "strength".into();
        strength.label = "Strength".into();
        h.state_mut().descriptor.params.params = vec![prompt, strength];
        h.run();
        let before_body = h.state().body;
        let before_track = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Strength").rect();
        let at = h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Prompt").rect().center();
        pointer_click(&mut h, at);
        h.run();
        assert_eq!(h.state().body, before_body, "opening a draft cannot change logical geometry");
        let track = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Strength").rect();
        assert!((track.top() - before_track.top()).abs() < 2.0 * zoom, "long editor displaced the following controls");
        assert!(h.state().body.shrink(crate::theme::DARK.card_padding * zoom).contains_rect(track));
        assert!(h.state().editor.text.contains_key(nodecraft_nodes::generation::PROMPT), "at={at:?}, body={before_body:?}, track={track:?}");
        assert_eq!(h.state().editor.text.get(nodecraft_nodes::generation::PROMPT).unwrap().text, text);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().intents.is_empty());
        assert!(h.state().editor.text.is_empty());
    }
}

#[test]
fn disabled_full_width_slider_cannot_publish_edits() {
    let mut h = harness(1.0, 2.0, slider(), false);
    let track = h.get_by_role(egui::accesskit::Role::Slider).rect();
    h.drag_at(track.center());
    h.run();
    h.hover_at(track.right_center());
    h.run();
    h.drop_at(track.right_center());
    h.run();
    assert!(!h.state().intents.iter().any(|intent| matches!(intent, Intent::Parameter { .. })));
    assert!(h.state().editor.dragging().is_none());
}

#[test]
fn broadphase_covers_full_section_budgets_and_capped_port_layout() {
    let t = &crate::theme::DARK;
    let mut node = Node::new(NodeId::from_bytes([81; 16]), nodecraft_nodes::generation::TEXT_KIND);
    let mut descriptor = descriptor(slider());
    descriptor.params.params = (0..100)
        .map(|i| {
            let mut spec = descriptor.params.params[0].clone();
            spec.name = format!("p{i}");
            spec
        })
        .collect();
    let asset = nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([7; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    node.history.push(nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([9; 16]), vec![Value::Media(asset)]));
    node.active = node.history.first().map(|generation| generation.id);
    for count in [0, 1, 25, 32, 256, 300] {
        node.outputs = (0..count).map(|i| port(format!("out{i}"))).collect();
        descriptor.outputs = node.outputs.clone();
        assert!(broadphase_size(&node, t).y >= world_size(&node, Some(&descriptor), t).y);
    }
    let capped = broadphase_size(&node, t);
    node.outputs.truncate(256);
    assert_eq!(broadphase_size(&node, t), capped, "hostile counts cannot widen layout beyond validated cap");
    assert_eq!(layout::Metrics::of(&node, Some(&descriptor), t).history, history::height(t) + t.control_gap);
}

#[test]
fn composer_budget_is_independent_of_legacy_creation_seed_height() {
    let mut t = crate::theme::DARK;
    let node = Node::new(NodeId::from_bytes([82; 16]), nodecraft_nodes::generation::TEXT_KIND);
    let mut descriptor = descriptor(ParamControl::Text { multiline: true, max_bytes: 4096 });
    descriptor.params.params[0].name = nodecraft_nodes::generation::PROMPT.into();
    let original = world_size(&node, Some(&descriptor), &t);
    t.card_min_height += 100.0;
    assert_eq!(world_size(&node, Some(&descriptor), &t), original);
    t.composer_body_height += 50.0;
    assert_eq!(world_size(&node, Some(&descriptor), &t).y, original.y + 50.0);
}

#[test]
fn long_choice_popup_scrolls_once_and_last_choice_remains_clickable() {
    let options = (0..80).map(|index| nodecraft_types::Choice { value: format!("v{index}"), label: format!("Choice {index:02}") }).collect();
    let mut h = harness(1.0, 1.0, ParamControl::Choice { options }, true);
    h.get_by_role(egui::accesskit::Role::ComboBox).click();
    h.run();
    let at = h.get_by_label("Choice 00").rect().center();
    h.hover_at(at);
    h.run();
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: Vec2::new(0.0, -10000.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    for _ in 0..20 {
        h.step();
    }
    let at = h.get_by_label("Choice 79").rect().center();
    assert!(h.ctx.viewport_rect().contains(at), "last choice must scroll into the visible popup");
    pointer_click(&mut h, at);
    assert_eq!(h.state().node.params.get("value"), Some(&Value::Text("v79".into())));
    assert!(
        h.state()
            .intents
            .iter()
            .any(|intent| matches!(intent, Intent::Parameter { name, value: Some(Value::Text(value)) } if name == "value" && value == "v79"))
    );
}
