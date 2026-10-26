//! Pasted ComfyUI JSON uses the same admission and transaction as a dropped workflow file.
use super::*;
use nodecraft_engine::import::{Incoming, import_files_positioned};

impl NodeCraftApp {
    /// Returns true for recognized workflow JSON, including a refused import. A failed workflow
    /// must report the import error rather than silently creating an unrelated Text card.
    pub(super) fn paste_workflow_text(&mut self, text: &str, centre: Pos2) -> bool {
        // A file's UTF-8 BOM is removed by the drop reader too. Ordinary pasted text keeps it.
        let workflow = text.strip_prefix('\u{feff}').unwrap_or(text);
        if !nodecraft_engine::comfy_workflow_cmds::is_workflow(workflow) {
            return false;
        }
        let Some(id) = ids::node_id(self.session.document(), ("paste-workflow", self.session.revision(), text.len())) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return true;
        };
        let t = self.theme.tokens();
        // egui's text clipboard event supplies no filename. File drops retain their own names.
        let incoming = [Incoming { name: "Pasted workflow", bytes: text.as_bytes() }];
        match import_files_positioned(
            &mut self.session,
            self.canvas,
            nodecraft_graph::Position { x: centre.x, y: centre.y },
            0.0,
            &incoming,
            &[id],
            &[],
            &|node, kinds| Self::creation_position(node, kinds, t),
        ) {
            Ok(imported) => {
                if !imported.nodes.is_empty() {
                    self.selection.only(id);
                }
                self.notice = imported.notice();
            }
            Err(error) => self.notice = Some(error.message),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use egui_kittest::Harness;
    use nodecraft_engine::comfy_workflow_cmds::{self, NodeCatalog};
    use std::sync::Arc;

    const INFO: &str = r#"{
      "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["IMAGE"]},
      "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true}
    }"#;
    const API: &str = r#"{
      "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
      "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "nc"}}
    }"#;
    const EDITOR: &str = r#"{
      "last_node_id": 2, "version": 0.4,
      "nodes": [
        {"id": 1, "type": "CLIPTextEncode", "mode": 0, "inputs": [], "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [1]}], "widgets_values": ["a red fox"]},
        {"id": 2, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 1}], "outputs": [], "widgets_values": ["nc"]}
      ],
      "links": [[1, 1, 0, 2, 0, "IMAGE"]]
    }"#;

    fn harness(read_nodes: bool) -> Harness<'static, NodeCraftApp> {
        let mut h = super::super::tests::harness();
        if read_nodes {
            let catalog = Arc::new(NodeCatalog::parse(INFO.as_bytes()).unwrap().0);
            comfy_workflow_cmds::finish(&mut h.state_mut().session, 1, Ok(catalog.into()));
        }
        h
    }

    fn paste(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
        h.hover_at(Pos2::new(200.0, 300.0));
        h.run();
        h.event(egui::Event::Paste(text.into()));
        h.run();
    }

    fn selected<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a Node {
        let app = h.state();
        let id = app.selection.nodes().first().unwrap();
        app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(id).unwrap()
    }

    #[test]
    fn pasted_api_and_editor_workflows_match_a_dropped_card_and_undo_once() {
        for workflow in [API, EDITOR] {
            let mut h = harness(true);
            let before = h.state().session.document().clone();
            paste(&mut h, workflow);
            let node = selected(&h).clone();
            assert_eq!(node.kind, nodecraft_nodes::comfy_workflow::ID);
            assert_eq!(node.title, "Pasted workflow");
            let card = nodecraft_nodes::comfy_workflow::card(&node).unwrap();
            assert!(card.mapping.params.iter().any(|param| param.input == "text"));
            assert_eq!(card.mapping.outputs.len(), 1);
            assert_eq!(h.state().session.revision(), 1);
            assert!(h.state().notice.is_none());
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &before);
            let canvas = h.state().canvas;
            nodecraft_engine::import::import_files(
                &mut h.state_mut().session,
                canvas,
                node.pos,
                0.0,
                &[nodecraft_engine::import::Incoming { name: "Pasted workflow", bytes: workflow.as_bytes() }],
                &[node.id],
            )
            .unwrap();
            let dropped = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&node.id).unwrap();
            assert_eq!(dropped, &node, "paste uses the drop's entire mapping and parameters");
        }
    }

    #[test]
    fn workflow_paste_without_server_nodes_shows_the_drops_notice_without_editing() {
        for workflow in [API, EDITOR] {
            let mut h = harness(false);
            let before = h.state().session.document().clone();
            paste(&mut h, workflow);
            assert_eq!(h.state().session.document(), &before);
            assert_eq!(h.state().session.revision(), 0);
            let notice = h.state().notice.clone().unwrap();
            assert!(notice.contains("Read ComfyUI server nodes"));
            let canvas = h.state().canvas;
            let dropped = nodecraft_engine::import::import_files(
                &mut h.state_mut().session,
                canvas,
                nodecraft_graph::Position { x: 0.0, y: 0.0 },
                0.0,
                &[nodecraft_engine::import::Incoming { name: "Pasted workflow", bytes: workflow.as_bytes() }],
                &[NodeId::from_bytes([77; 16])],
            )
            .unwrap();
            assert_eq!(Some(notice), dropped.notice());
        }
    }

    #[test]
    fn ordinary_text_and_other_json_keep_their_text_paste_behavior() {
        for text in ["a red fox", r#"{"prompt":"fox","seed":7}"#, "{broken JSON", r#"{"nodes":[{"id":"fox"}],"links":[]}"#] {
            let mut h = harness(true);
            let before = h.state().session.document().clone();
            paste(&mut h, text);
            assert_eq!(selected(&h).kind, "source.text");
            assert_eq!(selected(&h).params.get("text"), Some(&Value::Text(text.into())));
            assert_eq!(h.state().session.revision(), 1);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &before);
        }
    }

    #[test]
    fn a_copied_workflow_with_a_utf8_bom_is_recognized_like_its_file() {
        let mut h = harness(true);
        paste(&mut h, &format!("\u{feff}{API}"));
        assert_eq!(selected(&h).kind, nodecraft_nodes::comfy_workflow::ID);
        assert_eq!(h.state().session.revision(), 1);
    }

    #[test]
    fn an_unknown_workflow_node_does_not_fall_back_to_a_text_card() {
        let mut h = harness(true);
        let before = h.state().session.document().clone();
        paste(&mut h, &API.replace("SaveImage", "UnknownSave"));
        assert_eq!(h.state().session.document(), &before);
        assert!(h.state().notice.as_ref().unwrap().contains("UnknownSave"));
        assert_eq!(h.state().session.revision(), 0);
    }

    #[test]
    #[ignore = "manual local screenshot capture; requires NODECRAFT_PASTE946_RENDER_ROOT"]
    fn render_pasted_workflow_evidence() {
        let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PASTE946_RENDER_ROOT").unwrap());
        let mut h = harness(true);
        paste(&mut h, API);
        h.render().unwrap().save(root.join("pasted-workflow.png")).unwrap();
    }
}
