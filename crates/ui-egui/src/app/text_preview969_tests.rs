use super::*;
use egui_kittest::{Harness, kittest::Queryable};

pub(super) const ACTION: NodeId = NodeId::from_bytes([69; 16]);
const SECOND: NodeId = NodeId::from_bytes([70; 16]);

pub(super) fn harness(kind: &str, template: &str, first: &str, second: Option<&str>) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let source = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    app.session.dispatch("node.move", &json!({"canvas":canvas,"id":source,"pos":{"x":0,"y":0}})).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas":canvas,"id":source,"name":"text","value":Value::Text(first.into())})).unwrap();
    app.session.dispatch("node.create", &json!({"canvas":canvas,"id":ACTION,"kind":kind,"pos":{"x":420,"y":0}})).unwrap();
    let (port, name, parameter) = if kind == "text.template" { ("values", "template", template) } else { ("texts", "separator", "|") };
    app.session.dispatch("node.param_set", &json!({"canvas":canvas,"id":ACTION,"name":name,"value":Value::Text(parameter.into())})).unwrap();
    for (index, id) in std::iter::once(source).chain(second.map(|_| SECOND)).enumerate() {
        if id == SECOND {
            app.session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":"source.text","pos":{"x":0,"y":240}})).unwrap();
            app.session.dispatch("node.param_set", &json!({"canvas":canvas,"id":id,"name":"text","value":Value::Text(second.unwrap().into())})).unwrap();
        }
        app.session.dispatch("edge.connect", &json!({"canvas":canvas,"edge":{"id":nodecraft_types::EdgeId::from_bytes([90 + index as u8;16]),"from":{"node":id,"port":"out"},"to":{"node":ACTION,"port":port},"order":index}})).unwrap();
    }
    Harness::builder().with_size(Vec2::new(1400.0, 1100.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

pub(super) fn wait(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&Harness<'static, NodeCraftApp>) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        h.step();
        if done(h) {
            h.run();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("local preview did not arrive");
}

pub(super) fn has_result(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&ACTION).unwrap().active.is_some()
}

#[test]
fn join_and_template_show_ordered_inputs_and_the_computed_output_and_follow_edits() {
    for (kind, expected, port) in [("text.concat", "alpha|beta", "texts"), ("text.template", "Hello alpha / beta", "values")] {
        let mut h = harness(kind, "Hello {{1}} / {{2}}", "alpha", Some("beta"));
        wait(&mut h, has_result);
        h.get_by_label(expected);
        h.get_by_label(&format!("Input {port} · 1 · Text"));
        h.get_by_label(&format!("Input {port} · 2 · Text"));
        h.get_by_label("alpha");
        h.get_by_label("beta");
        let canvas = h.state().canvas;
        h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":SECOND,"name":"text","value":Value::Text("gamma".into())})).unwrap();
        let expected = expected.replace("beta", "gamma");
        wait(&mut h, |h| h.query_by_label(&expected).is_some());
        h.state_mut().view.zoom = 0.6;
        h.run();
        h.get_by_label(&expected);
    }
}

#[test]
fn empty_values_and_missing_inputs_are_visible_on_the_card() {
    let mut h = harness("text.concat", "", "", None);
    wait(&mut h, has_result);
    assert_eq!(h.query_all_by_label("(empty)").count(), 2, "both the input and output explain the empty value");
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas":canvas,"id":nodecraft_types::EdgeId::from_bytes([90;16])})).unwrap();
    h.run();
    h.get_by_label("Input texts");
    h.get_by_label("Connect an input first");
}

#[test]
fn template_errors_stay_on_their_card_and_disappear_when_fixed() {
    let mut h = harness("text.template", "{{3}}", "alpha", Some("beta"));
    wait(&mut h, |h| h.query_by_label("Error (values):").is_some());
    assert!(h.query_all_by_label_contains("template field is missing").next().is_some());
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":canvas,"id":ACTION,"name":"template","value":Value::Text("Hi {{1}}".into())})).unwrap();
    wait(&mut h, |h| h.query_by_label("Hi alpha").is_some());
    assert!(h.query_by_label("Error (values):").is_none());
}

#[test]
fn expand_shows_full_unicode_multiline_text_and_owns_canvas_shortcuts() {
    let text = format!("{}\nsecond\nthird\nfourth tail", "🦊 café ".repeat(100));
    let mut h = harness("text.template", "{{1}}", &text, None);
    wait(&mut h, has_result);
    assert!(h.query_by_label(&text).is_none(), "the full large value isn't laid out in the card");
    h.get_by_label("Expand Output out").click();
    h.run();
    h.get_by_label(&text);
    let before = h.state().session.document().clone();
    h.key_press(egui::Key::Delete);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    h.get_by_label("Close text preview").click();
    h.run();
    assert!(h.query_by_label("Close text preview").is_none());
}

#[test]
#[ignore = "manual public-safe capture; NODECRAFT_TEXT969_RENDER_ROOT"]
fn render_local_text_previews() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TEXT969_RENDER_ROOT").unwrap());
    for (kind, name) in [("text.concat", "join"), ("text.template", "template")] {
        let mut h = harness(kind, "A story of {{1}} and {{2}}", "a curious fox", Some("a sleepy owl"));
        wait(&mut h, has_result);
        h.render().unwrap().save(root.join(format!("{name}.png"))).unwrap();
    }
}

#[test]
fn batch_items_include_their_upstream_values_and_own_text_in_both_actions() {
    for (kind, port, expected) in [("text.concat", "texts", "alpha|gamma|delta"), ("text.template", "values", "alpha + gamma + delta")] {
        let mut h = harness(kind, "{{1}} + {{2}} + {{3}}", "alpha", None);
        let canvas = h.state().canvas;
        let source = *h.state().session.document().canvases().get(&canvas).unwrap().nodes().keys().find(|id| **id != ACTION).unwrap();
        h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas":canvas,"id":EdgeId::from_bytes([90;16])})).unwrap();
        let batch = NodeId::from_bytes([110; 16]);
        h.state_mut().session.dispatch("node.create", &json!({"canvas":canvas,"id":batch,"kind":"structure.batch","pos":{"x":100,"y":500}})).unwrap();
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas":canvas,"id":batch,"name":"text","value":Value::Text("gamma\ndelta".into())}))
            .unwrap();
        for (edge, from, output, to, input) in [(111, source, "out", batch, "in"), (112, batch, "item", ACTION, port)] {
            h.state_mut().session.dispatch("edge.connect", &json!({"canvas":canvas,"edge":{"id":EdgeId::from_bytes([edge;16]),"from":{"node":from,"port":output},"to":{"node":to,"port":input},"order":0}})).unwrap();
        }
        wait(&mut h, |h| h.query_by_label(expected).is_some());
        h.get_by_label("alpha\ngamma\ndelta");
        h.get_by_label(&format!("Expand Input {port} · 1 · Batch")).click();
        h.run();
        h.get_by_label("Close text preview");
        assert!(h.query_all_by_label("alpha\ngamma\ndelta").count() >= 1);
    }
}
