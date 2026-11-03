//! Original NodeCraft starter-data author. This module is test/tooling only; shipped workflows are .ncraft data.
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Position, Project, Technique, TechniquePort};
use nodecraft_nodes::NodeKind;
use nodecraft_types::{AssetRef, CanvasId, EdgeId, Modality, NodeId, ParamValues, ProjectId, TechniqueId, Unsigned, Value};
use serde_json::json;
use std::collections::BTreeMap;

pub struct Example {
    pub id: String,
    pub title: String,
    pub description: String,
    pub snapshot: Snapshot,
    pub features: Vec<String>,
    pub canvas: CanvasId,
}
struct Graph {
    id: String,
    title: String,
    description: String,
    project: Project,
    assets: AssetStore,
    kinds: nodecraft_nodes::Registry,
    canvas: CanvasId,
    next: u32,
}
fn ident(id: &str, n: u32) -> [u8; 16] {
    let hash = blake3::hash(format!("nodecraft.template.v1/{id}/{n}").as_bytes());
    hash.as_bytes()[..16].try_into().unwrap()
}
fn t(s: &str) -> Value {
    Value::Text(s.into())
}
fn u(n: u64) -> Value {
    Value::Unsigned(Unsigned(n))
}
impl Graph {
    fn new(id: &str, title: &str, description: &str) -> Self {
        let mut project = Project::new(ProjectId::from_bytes(ident(id, 0)), title).unwrap();
        let canvas = CanvasId::from_bytes(ident(id, 1));
        Change::AddCanvas { canvas: Canvas::new(canvas, title) }.apply(&mut project).unwrap();
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            project,
            assets: AssetStore::default(),
            kinds: GenerationCatalog::builtin().unwrap().registry().unwrap(),
            canvas,
            next: 2,
        }
    }
    fn node(&mut self, kind: &str, values: &[(&str, Value)]) -> NodeId {
        self.configured(kind, values, BTreeMap::new())
    }
    fn configured(&mut self, kind: &str, values: &[(&str, Value)], config: BTreeMap<String, Value>) -> NodeId {
        let id = NodeId::from_bytes(ident(&self.id, self.next));
        self.next += 1;
        let params = values.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<ParamValues>();
        let mut node = self.kinds.get(kind).unwrap().instantiate_instance(id, config, &params).unwrap();
        node.pos = Position { x: ((self.next - 3) % 3) as f32 * 350.0, y: ((self.next - 3) / 3) as f32 * 270.0 };
        node.title = format!("{} {}", node.title, self.next - 2);
        Change::AddNode { canvas: self.canvas, node }.apply(&mut self.project).unwrap();
        id
    }
    fn wire(&mut self, from: NodeId, out: &str, to: NodeId, input: &str, order: u32) {
        let many =
            self.project.canvases().get(&self.canvas).unwrap().nodes().get(&to).unwrap().inputs.iter().find(|port| port.name == input).unwrap().port_type.many;
        let order = if many { order } else { 0 };
        let edge = Edge {
            id: EdgeId::from_bytes(ident(&self.id, self.next + 1000)),
            from: Endpoint { node: from, port: out.into() },
            to: Endpoint { node: to, port: input.into() },
            order,
        };
        self.next += 1;
        Change::Connect { canvas: self.canvas, edge }.apply(&mut self.project).unwrap_or_else(|e| panic!("{}: {input}: {e}", self.id));
    }
    fn image(&mut self, color: u8) -> NodeId {
        // Opaque green background plus an asymmetric L and a separate colored square:
        // flips/rotation/blur are visible, and the default green key has something to remove.
        let mut rgba = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                let pixel = if ((8..18).contains(&x) && (6..48).contains(&y)) || ((8..44).contains(&x) && (38..48).contains(&y)) {
                    [240, color, 32, 255]
                } else if (35..55).contains(&x) && (10..28).contains(&y) {
                    [32, 80, color.saturating_add(60), 255]
                } else {
                    [0, 255, 0, 255]
                };
                rgba.extend_from_slice(&pixel);
            }
        }
        let bytes = nodecraft_media::encode_png(64, 64, &rgba, &Default::default()).unwrap();
        self.media("source.image", "image/png", &bytes)
    }
    fn media(&mut self, kind: &str, mime: &str, bytes: &[u8]) -> NodeId {
        let hash = self.assets.insert(bytes).unwrap();
        let meta = if mime.starts_with("image/") {
            nodecraft_format::image_header_meta(&nodecraft_media::probe_image(bytes, &Default::default()).unwrap())
        } else {
            Default::default()
        };
        self.node(kind, &[("asset", Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta }))])
    }
    fn text(&mut self, text: &str) -> NodeId {
        self.node("source.text", &[("text", t(text))])
    }
    fn generation(&mut self, kind: &str, model: &str, prompt: &str) -> NodeId {
        self.node(kind, &[("model", t(model)), ("prompt", t(prompt))])
    }
    fn finish(mut self, features: &[&str]) -> Example {
        let text = format!(
            "{}\n\n{}\n\nEdit the named source or settings, then use Run all. Cloud runs require your own key and cost consent; tests are offline. Source fixtures are original tiny NodeCraft examples.",
            self.title, self.description
        );
        self.node("structure.note", &[("text", Value::Text(text))]);
        self.project.validate().unwrap();
        self.assets.validate_project(&self.project).unwrap();
        Example {
            id: self.id,
            title: self.title,
            description: self.description,
            snapshot: Snapshot { project: self.project, assets: self.assets, epoch: 0 },
            features: features.iter().map(|s| s.to_string()).collect(),
            canvas: self.canvas,
        }
    }
}
fn technique(id: &str, title: &str, description: &str) -> Example {
    let mut g = Graph::new(id, title, description);
    let input = g.text("A paper boat");
    let action = g.node("text.template", &[("template", t("A calm illustration of {{1}}"))]);
    g.wire(input, "out", action, "values", 0);
    let source = g.project.canvases().get(&g.canvas).unwrap();
    let input_node = source.nodes().get(&input).unwrap().clone();
    let output_node = source.nodes().get(&action).unwrap().clone();
    let version = CanvasId::from_bytes(ident(id, 200));
    let port = |node: NodeId, name: &str| TechniquePort {
        name: name.into(),
        description: format!("Editable {name}"),
        node,
        port: "out".into(),
        port_type: nodecraft_types::PortType { modality: Modality::Text, many: false },
    };
    let info = Technique {
        id: TechniqueId::from_bytes(ident(id, 201)),
        version: 1,
        name: title.into(),
        description: description.into(),
        category: "Starters".into(),
        tags: vec!["starter".into()],
        inputs: vec![port(input, "Subject")],
        outputs: vec![port(action, "Prompt")],
        params: vec![],
        source: None,
        retired: false,
    };
    // The published copy has its own globally unique node/edge identities.
    let mut a = input_node;
    a.id = NodeId::from_bytes(ident(id, 202));
    let mut b = output_node;
    b.id = NodeId::from_bytes(ident(id, 203));
    let info = Technique { inputs: vec![port(a.id, "Subject")], outputs: vec![port(b.id, "Prompt")], ..info };
    let edge = Edge {
        id: EdgeId::from_bytes(ident(id, 204)),
        from: Endpoint { node: a.id, port: "out".into() },
        to: Endpoint { node: b.id, port: "values".into() },
        order: 0,
    };
    let canvas = Canvas::technique(version, format!("{title} v1"), info, vec![a, b], vec![edge]).unwrap();
    let interface = nodecraft_nodes::technique::interface_of(&canvas, &g.kinds).unwrap();
    Change::AddCanvas { canvas }.apply(&mut g.project).unwrap();
    let placed = g.configured("structure.technique", &[], interface.config().unwrap());
    if id != "app-form" {
        g.wire(input, "out", placed, "subject", 0);
    }
    g.finish(&["technique", "published-technique", "app-form"])
}

pub fn examples() -> Vec<Example> {
    let mut out = vec![];
    for (id, title, kind, model, prompt, feature) in [
        ("generate-text", "Generate text", "generation.text", "openai.gpt-4.1-mini", "Describe a paper boat in one sentence.", "text-generation"),
        ("generate-image", "Generate an image", "generation.image", "fal.flux-schnell", "A blue paper boat on a quiet pond", "image-generation"),
        ("generate-video", "Generate a video", "generation.video", "fal.wan22", "A paper boat drifting across a quiet pond", "video-generation"),
        ("generate-audio", "Generate speech", "generation.audio", "fal.kokoro", "A paper boat floats on a quiet pond.", "audio-generation"),
        ("generate-3d", "Generate a 3D model", "generation.mesh", "tripo.text-to-3d", "A simple low-poly paper boat", "mesh-generation"),
        ("cloud-local", "Switch cloud and local", "generation.image", "fal.flux-schnell", "A blue paper boat on a quiet pond", "cloud-local-switch"),
    ] {
        let description = if id == "cloud-local" {
            "The prompt wire stays connected when changing this Image card from fal FLUX Schnell to local ComfyUI SDXL. Select the local model and set its checkpoint in your ComfyUI setup."
        } else {
            "Change the source prompt, choose a compatible model, review the cost and run the connected generation card."
        };
        let mut g = Graph::new(id, title, description);
        let source = g.text(prompt);
        let generated = g.generation(kind, model, "");
        g.wire(source, "out", generated, "prompt", 0);
        out.push(g.finish(&[feature]));
    }
    {
        let mut g = Graph::new(
            "documents",
            "Read a PDF",
            "Extract original PDF text and page images. Replace the tiny bundled PDF with your document, then adjust the page range. Document generation is not implemented; this demonstrates available document handling.",
        );
        let bytes = nodecraft_media::pdf_fixture::pdf(&["BT /F1 18 Tf 20 100 Td (A paper boat) Tj ET"]);
        let source = g.media("source.document", "application/pdf", &bytes);
        for (kind, port) in [("document.text", "document"), ("document.pages", "document")] {
            let n = g.node(kind, &[]);
            g.wire(source, "out", n, port, 0);
        }
        out.push(g.finish(&["document-source", "pdf-text", "pdf-images"]));
    }
    {
        let mut g = Graph::new(
            "document-prompt",
            "Ask about a document",
            "A PDF feeds a document-aware text model. Replace the bundled one-page PDF and edit the question; it stays an original document input.",
        );
        let bytes = nodecraft_media::pdf_fixture::pdf(&["BT /F1 18 Tf 20 100 Td (A paper boat) Tj ET"]);
        let source = g.media("source.document", "application/pdf", &bytes);
        let n = g.generation("generation.text", "openai.gpt-4.1-mini", "Summarize the attached document.");
        g.wire(source, "out", n, "document", 0);
        out.push(g.finish(&["document-aware-generation"]));
    }
    {
        let mut g = Graph::new(
            "combine",
            "Combine images",
            "Two images feed both a shared edit model and a Layer Editor. Replace the sources, compare the model's combination with the local composition, or edit the layer layout. Originals stay unchanged.",
        );
        let a = g.image(80);
        let b = g.image(180);
        let n = g.node("image.layers", &[("width", u(256)), ("height", u(256))]);
        g.wire(a, "out", n, "layers", 0);
        g.wire(b, "out", n, "layers", 1);
        let generated = g.generation("generation.image", "fal.grok-image2-edit", "Combine the two reference pictures into one calm scene.");
        g.wire(a, "out", generated, "images", 0);
        g.wire(b, "out", generated, "images", 1);
        out.push(g.finish(&["combine", "layer-editor"]));
    }
    {
        let mut g = Graph::new(
            "branch",
            "Branch an image",
            "One source feeds two independent edits. Change the tint or rotation and compare both results; the original stays unchanged.",
        );
        let a = g.image(120);
        for kind in ["image.tint", "image.rotate"] {
            let n = g.node(kind, &[]);
            g.wire(a, "out", n, "image", 0);
        }
        out.push(g.finish(&["branch"]));
    }
    {
        let mut g = Graph::new(
            "prompt-helper",
            "Write an image-guided prompt",
            "An image feeds a text model that writes the downstream edit prompt. The same original also feeds the edit model. Replace the source or change the text instruction; the generated text controls the connected image prompt.",
        );
        let source = g.image(120);
        let helper = g.generation("generation.text", "openai.gpt-4.1-mini", "Write a short image-edit instruction inspired by this picture.");
        let generated = g.generation("generation.image", "fal.grok-image2-edit", "");
        g.wire(source, "out", helper, "image", 0);
        g.wire(helper, "out", generated, "prompt", 0);
        g.wire(source, "out", generated, "images", 0);
        out.push(g.finish(&["prompt-helper", "image-guided-prompt"]));
    }
    for (id, title, split, text) in
        [("csv-batch", "Batch from CSV", "csv", "prompt\nPaper boat\nPaper kite"), ("prompt-list", "Batch a prompt list", "line", "Paper boat\nPaper kite")]
    {
        let mut g = Graph::new(
            id,
            title,
            "Edit the Batch items. CSV uses the first column after its header; a prompt list uses one line per item. The downstream text step runs once per item.",
        );
        let b = g.node("structure.batch", &[("text", t(text)), ("split", t(split))]);
        let n = g.generation("generation.text", "openai.gpt-4.1-mini", "Write a one-sentence illustration prompt.");
        g.wire(b, "item", n, "prompt", 0);
        out.push(g.finish(&[id]));
    }
    for pairing in ["cross", "zip"] {
        let id = format!("batch-{pairing}");
        let mut g = Graph::new(
            &id,
            &format!("Batch {pairing}"),
            "Two batches reach one text template. Cross produces all four combinations; Zip pairs the first items and the second items. Edit the two lists; Zip needs equal lengths.",
        );
        let a = g.node("structure.batch", &[("text", t("Boat\nKite")), ("pairing", t(pairing))]);
        let b = g.node("structure.batch", &[("text", t("Blue\nOrange")), ("pairing", t(pairing))]);
        // A single-input step after each batch preserves fan-out before a list input could collect it.
        let left = g.generation("generation.text", "openai.gpt-4.1-mini", "");
        let right = g.generation("generation.text", "openai.gpt-4.1-mini", "");
        g.wire(a, "item", left, "prompt", 0);
        g.wire(b, "item", right, "prompt", 0);
        let n = g.generation("generation.text", "openai.gpt-4.1-mini", "Combine these two ideas.");
        g.wire(left, "out", n, "prompt", 0);
        g.wire(right, "out", n, "prompt", 1);
        out.push(g.finish(&[&id]));
    }
    out.push(technique(
        "technique",
        "Use a Technique",
        "A published prompt helper runs as one Technique node. Edit its wired subject, then Run all. View workflow opens the pinned version.",
    ));
    out.push(technique("published-technique","Publish and reuse","The project includes an immutable published version and its editable source workflow. In Techniques, add the version to your library, then place it in another project."));
    out.push(technique(
        "app-form",
        "Use an app form",
        "Select the Technique node and open App mode. Its Subject input becomes a generated form; edit it and run to see the Prompt output.",
    ));
    {
        let mut g = Graph::new(
            "router",
            "Route a shared prompt",
            "A Router is a transparent hub with no provider or cost. Replace the source text; both consumers receive it through the same Router.",
        );
        let a = g.text("A blue paper boat");
        let r = g.node("structure.router", &[]);
        g.wire(a, "out", r, "in", 0);
        for suffix in ["in daylight", "at dusk"] {
            let n = g.node("text.template", &[("template", t(&format!("{{{{1}}}} {suffix}")))]);
            g.wire(r, "out", n, "values", 0);
        }
        out.push(g.finish(&["router"]));
    }
    {
        use nodecraft_nodes::comfy_workflow::{Card, MappedOutput, MappedParam, Mapping};
        let mut g = Graph::new(
            "comfy-core",
            "Mapped core ComfyUI workflow",
            "An original six-node core-only sampler workflow. Edit prompt and checkpoint on the card; choose a configured server. Offline tests validate mapping and execution contracts; no live server or model file is bundled or probed.",
        );
        let workflow = json!({"1":{"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"v1-5-pruned-emaonly.safetensors"}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":"A blue paper boat","clip":["1",1]}},"3":{"class_type":"EmptyLatentImage","inputs":{"width":512,"height":512,"batch_size":1}},"4":{"class_type":"KSampler","inputs":{"seed":1,"steps":20,"cfg":7.0,"sampler_name":"euler","scheduler":"normal","denoise":1.0,"model":["1",0],"positive":["2",0],"negative":["2",0],"latent_image":["3",0]}},"5":{"class_type":"VAEDecode","inputs":{"samples":["4",0],"vae":["1",2]}},"6":{"class_type":"SaveImage","inputs":{"filename_prefix":"NodeCraft-starter","images":["5",0]}}});
        let spec = |name: &str, default: Value| nodecraft_types::ParamSpec {
            name: name.into(),
            label: name.into(),
            description: Some("Edit this core workflow input".into()),
            control: nodecraft_types::ParamControl::Text { multiline: false, max_bytes: 4096 },
            required: false,
            default: Some(default),
            visible_if: None,
        };
        let card = Card {
            workflow: workflow.to_string(),
            mapping: Mapping {
                inputs: vec![],
                params: vec![
                    MappedParam { node: "2".into(), input: "text".into(), spec: spec("prompt", t("A blue paper boat")) },
                    MappedParam { node: "1".into(), input: "ckpt_name".into(), spec: spec("checkpoint", t("v1-5-pruned-emaonly.safetensors")) },
                ],
                outputs: vec![MappedOutput { node: "6".into(), label: "Saved image".into(), modality: Modality::Image }],
                partners: vec![],
                output_names: vec![],
            },
        };
        card.mapping.check_targets(&nodecraft_comfy::ApiWorkflow::parse(card.workflow.as_bytes(), None).unwrap()).unwrap();
        g.configured("generation.comfy_workflow", &[], card.config().unwrap());
        out.push(g.finish(&["mapped-comfy-workflow"]));
    }
    // One small graph for every currently registered deterministic image/text action.
    for kind in nodecraft_nodes::action::kinds().unwrap() {
        let d = kind.descriptor();
        let id = format!("action-{}", d.id.replace('.', "-"));
        let mut g = Graph::new(
            &id,
            &d.label,
            &format!("{} runs locally on tiny original examples. Edit its visible controls or replace a source. No provider key is needed.", d.label),
        );
        let n = g.node(&d.id, &[]);
        for (order, port) in d.inputs.iter().enumerate() {
            let source =
                if port.port_type.modality == Modality::Image { g.image(80 + u8::try_from(order).unwrap() * 70) } else { g.text("Paper boat,Paper kite") };
            g.wire(source, "out", n, &port.name, 0);
            if d.id == "text.concat" {
                let second = g.text("A bright paper kite");
                g.wire(second, "out", n, &port.name, 1);
            }
        }
        out.push(g.finish(&[&d.id]));
    }
    {
        let mut g = Graph::new(
            "layout",
            "Design a layout",
            "A Layer Editor holds original text and shape layers at a small canvas size. Open its editor, change the title or move the shape, then run to render a new image.",
        );
        let config=[(nodecraft_nodes::layers::CONFIG.into(),t(r##"{"layers":[{"id":"title","name":"Title","x":128,"y":64,"width":220,"height":50,"content":{"kind":"text","text":"Paper boats","size":28,"color":"#102840"}},{"id":"shape","name":"Badge","x":128,"y":160,"width":100,"height":70,"content":{"kind":"shape","shape":"rectangle","fill":"#4080c0"}}]}"##))].into();
        g.configured("image.layers", &[("width", u(256)), ("height", u(256))], config);
        out.push(g.finish(&["layout", "text-layers", "shape-layers"]));
    }
    {
        let mut g = Graph::new(
            "mesh-convert",
            "Convert a 3D model",
            "An original triangle is kept as GLB. Change Format to OBJ to make a copy for another editor; the source and its original bytes stay unchanged.",
        );
        let bytes = include_bytes!("../../../providers/src/fal/fixtures/triangle.glb");
        let a = g.media("source.mesh", "model/gltf-binary", bytes);
        let n = g.node("mesh.convert", &[]);
        g.wire(a, "out", n, "model", 0);
        out.push(g.finish(&["mesh-source", "mesh-convert"]));
    }
    {
        let mut g = Graph::new(
            "elements",
            "Reuse a subject",
            "Two images become one named Element reference sheet. Replace the pictures and edit its name or guidelines, then run; mention the saved subject in a downstream prompt.",
        );
        let a = g.image(80);
        let b = g.image(180);
        let e = g.node("element", &[("name", t("PaperBoat"))]);
        g.wire(a, "out", e, "images", 0);
        g.wire(b, "out", e, "images", 1);
        out.push(g.finish(&["element-reference"]));
    }
    for (id, title) in [("storyboard", "Present a storyboard"), ("generate-document", "Generate a PDF document")] {
        let mut g = Graph::new(
            id,
            title,
            if id == "storyboard" {
                "Three original image sources form a storyboard Deck. Replace the images, edit slide order, and use Present or Export PDF. Each slide follows its source card."
            } else {
                "This Deck generates a new PDF document through deck.export. Edit its original text layers in Edit slides, then Export PDF. It is local document creation, not an AI document-generation model."
            },
        );
        let mut slides = vec![];
        for i in 0..if id == "storyboard" { 3 } else { 1 } {
            let content = if id == "storyboard" {
                let source = g.image(60 + i * 50);
                json!({"kind":"source","origin":source.to_string(),"nth":0,"fit":true})
            } else {
                json!({"kind":"text","text":"Paper boats — a tiny document","size":18,"color":"#102840"})
            };
            slides.push(json!({"id":format!("slide-{i}"),"name":format!("Scene {}",i+1),"layers":[{"id":format!("layer-{i}"),"name":"Scene","x":128,"y":128,"width":230,"height":230,"content":content}]}));
        }
        g.configured(
            "structure.deck",
            &[("width", u(256)), ("height", u(256))],
            [(nodecraft_nodes::deck::CONFIG.into(), t(&json!({"version":1,"slides":slides}).to_string()))].into(),
        );
        out.push(g.finish(&[id, "deck-present", "deck-export"]));
    }
    for kind in ["image.mask", "image.shader"] {
        let id = format!("action-{}", kind.replace('.', "-"));
        let mut g = Graph::new(
            &id,
            if kind == "image.mask" { "Paint a mask" } else { "Use a custom shader" },
            "The original image feeds an editable local tool. Open the card editor, change its settings and run. The source remains unchanged.",
        );
        let source = g.image(140);
        let n = g.node(kind, &[]);
        g.wire(source, "out", n, "image", 0);
        out.push(g.finish(&[kind]));
    }
    for kind in ["mesh.photo", "mesh.views", "mesh.turntable"] {
        let mut g = Graph::new(
            &format!("action-{}", kind.replace('.', "-")),
            kind,
            "An original GLB triangle feeds this local 3D capture. Edit its camera and look; rendered copies preserve the original model. Native GPU required; turntable also needs configured FFmpeg.",
        );
        let source = g.media("source.mesh", "model/gltf-binary", include_bytes!("../../../providers/src/fal/fixtures/triangle.glb"));
        let params = match kind {
            "mesh.photo" => vec![("width", u(64)), ("height", u(64))],
            "mesh.views" => vec![("size", u(64)), ("count", t("4"))],
            _ => vec![("width", u(64)), ("height", u(64)), ("fps", u(4)), ("duration", Value::Number(nodecraft_types::Number::new(0.5).unwrap()))],
        };
        let n = g.node(kind, &params);
        g.wire(source, "out", n, "model", 0);
        out.push(g.finish(&[kind]));
    }
    let mut video_kinds = nodecraft_nodes::video_edit::kinds().unwrap();
    video_kinds.extend(nodecraft_nodes::video_frames::kinds().unwrap());
    video_kinds.extend(nodecraft_nodes::video_look::kinds().unwrap());
    video_kinds.extend(nodecraft_nodes::video_split::kinds().unwrap());
    for kind in video_kinds {
        let d = kind.descriptor();
        let mut g = Graph::new(
            &format!("action-{}", d.id.replace('.', "-")),
            &d.label,
            &if d.id == "video.stitch" {
                "Play the original moving marker, then its reversed clip. Swap the two wire orders or change the transition. Configure FFmpeg/ffprobe in media settings; no cloud call is needed.".into()
            } else {
                format!(
                    "{} edits an original one-second moving marker on blue with a tone. Replace a source and edit its controls. Configure FFmpeg/ffprobe in media settings; no cloud call is needed.",
                    d.label
                )
            },
        );
        let mut overrides = vec![];
        for spec in &d.params.params {
            if spec.name == "size" {
                overrides.push(("size", u(64)));
            }
            if spec.name == "cell" {
                overrides.push(("cell", u(64)));
            }
            if spec.name == "frames" {
                overrides.push(("frames", u(2)));
            }
        }
        let n = g.node(&d.id, &overrides);
        for (i, port) in d.inputs.iter().enumerate() {
            let source = match port.port_type.modality {
                Modality::Image => g.image(100),
                Modality::Audio => g.media("source.audio", "audio/wav", include_bytes!("../../../providers/src/fal/fixtures/tone.wav")),
                _ => g.media("source.video", "video/mp4", nodecraft_template_data::VIDEO_FIXTURE),
            };
            g.wire(source, "out", n, &port.name, u32::try_from(i).unwrap());
            if d.id == "video.stitch" {
                let backwards = g.node("video.reverse", &[]);
                g.wire(source, "out", backwards, "video", 0);
                g.wire(backwards, "out", n, &port.name, 1);
            }
        }
        out.push(g.finish(&[&d.id]));
    }
    {
        let mut g = Graph::new(
            "timeline",
            "Arrange a Timeline",
            "A one-second original clip becomes an automatic Timeline clip. Open the editor to trim or add sources, then run. Configure FFmpeg/ffprobe; all media is bundled.",
        );
        let source = g.media("source.video", "video/mp4", nodecraft_template_data::VIDEO_FIXTURE);
        let n = g.node("video.timeline", &[("width", u(64)), ("height", u(64))]);
        g.wire(source, "out", n, "clips", 0);
        out.push(g.finish(&["timeline"]));
    }
    {
        let mut g = Graph::new(
            "train-style",
            "Train a reusable style",
            "Four original small references feed fal's FLUX style trainer, whose style feeds a compatible Image card. Replace all references, then review both the training quote and the usage-priced image cost. The training result remains a reusable style descriptor.",
        );
        let n = g.generation("generation.style", "fal.flux-lora-trainer", "");
        for i in 0..4 {
            let source = g.image(50 + i * 40);
            g.wire(source, "out", n, "images", u32::from(i));
        }
        let styled = g.generation("generation.image", "fal.flux-lora", "A paper boat in the trained paper style");
        g.wire(n, "out", styled, "style", 0);
        out.push(g.finish(&["style-training", "style-application"]));
    }
    {
        let mut g = Graph::new(
            "folder",
            "Loop over a folder",
            "Two bundled text files demonstrate a Folder card's names, filters, natural order and per-file outputs. Edit Include, Sort or Range; importing a real folder later keeps its files in the project.",
        );
        let config = nodecraft_nodes::folder::config(
            Modality::Text,
            "Original examples",
            vec![("scene-1.txt".into(), t("Paper boat")), ("scene-2.txt".into(), t("Paper kite"))],
        )
        .unwrap();
        let f = g.configured("source.folder", &[], config);
        let n = g.generation("generation.text", "openai.gpt-4.1-mini", "Describe this scene in one sentence.");
        g.wire(f, "file", n, "prompt", 0);
        out.push(g.finish(&["folder", "folder-filter", "folder-range"]));
    }
    for (id, title, kind, model, role, mime, sourcekind) in [
        ("edit-image", "Edit an image", "generation.image", "fal.grok-image2-edit", "images", "image/png", "source.image"),
        ("image-to-video", "Animate an image", "generation.video", "fal.wan22-i2v", "first_frame", "image/png", "source.image"),
        ("image-to-3d", "Build 3D from an image", "generation.mesh", "fal.trellis2", "image", "image/png", "source.image"),
        ("transcribe-audio", "Transcribe a recording", "generation.text", "deepgram.nova-3", "audio", "audio/wav", "source.audio"),
    ] {
        let mut g = Graph::new(
            id,
            title,
            "Replace the original tiny source and edit the prompt where supported. The existing typed wire chooses the correct generation mode. Review the model's cost before a cloud run.",
        );
        let source = if mime == "image/png" { g.image(120) } else { g.media(sourcekind, mime, include_bytes!("../../../providers/src/fal/fixtures/tone.wav")) };
        let prompt = if id == "transcribe-audio" { "" } else { "A blue paper boat" };
        let n = g.generation(kind, model, prompt);
        g.wire(source, "out", n, role, 0);
        out.push(g.finish(&[id]));
    }
    {
        let mut g = Graph::new(
            "generation-sheet",
            "Generate in a sheet",
            "Two Image cards share a table canvas; the first requests two variations, the second one. Edit each prompt/model row and review all three request costs; results remain in each card's history.",
        );
        let mut plain = serde_json::to_value(g.project.canvases().get(&g.canvas).unwrap()).unwrap();
        plain["kind"] = json!("sheet");
        let canvas: Canvas = serde_json::from_value(plain).unwrap();
        Change::RemoveCanvas { id: g.canvas }.apply(&mut g.project).unwrap();
        Change::AddCanvas { canvas }.apply(&mut g.project).unwrap();
        for (index, prompt) in ["A blue paper boat", "An orange paper kite"].into_iter().enumerate() {
            g.configured(
                "generation.image",
                &[("model", t("fal.flux-schnell")), ("prompt", t(prompt))],
                if index == 0 { [(nodecraft_exec::variations::VARIATIONS_CONFIG.into(), u(2))].into() } else { Default::default() },
            );
        }
        out.push(g.finish(&["generation-sheet", "variations"]));
    }
    out
}

#[test]
#[ignore = "data author: TEMPLATE952_WRITE_DIR=external repository; no provider or network"]
fn templates952_author() {
    let dir = std::path::PathBuf::from(std::env::var_os("TEMPLATE952_WRITE_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let mut index = vec![];
    for example in examples() {
        let bytes = nodecraft_format::encode(&example.snapshot).unwrap();
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let file = format!("templates/{}/1/{digest}.ncraft", example.id);
        let path = dir.join(&file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &bytes).unwrap();
        index.push(json!({"id":example.id,"title":example.title,"description":example.description,"version":1,"file":file,"blake3":digest,"features":example.features,"canvas":example.canvas}));
    }
    std::fs::write(dir.join("index.json"), serde_json::to_vec_pretty(&json!({"schema_version":1,"templates":index})).unwrap()).unwrap();
}
