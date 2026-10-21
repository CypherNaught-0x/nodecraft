//! Style training and styled images on fal (#167), against recorded-shape fixtures.
use super::tests::{job, png, receipt};
use super::*;
use base64::Engine;
use futures::executor::block_on;
use serde_json::json;
use std::{collections::VecDeque, io::Read, sync::Mutex};

const KEY: &str = "fal-fixture-key-not-real";
const WEIGHTS: &str = "https://v3.fal.media/files/lion/abc_pytorch_lora_weights.safetensors";

struct Transport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}
impl Transport {
    fn new(replies: Vec<serde_json::Value>) -> Arc<Self> {
        let replies = replies.into_iter().map(|v| HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() }).collect();
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies) })
    }
    fn push(&self, body: Vec<u8>) {
        self.replies.lock().unwrap().push_back(HttpResponse { status: 200, headers: vec![], body });
    }
    fn body(&self, n: usize) -> serde_json::Value {
        serde_json::from_slice(&self.requests.lock().unwrap()[n].body).unwrap()
    }
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("legacy send must not run")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(r);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { Ok(reply) })
    }
}
fn fal(t: &Arc<Transport>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), t.clone())
}
fn image(n: u32) -> crate::JobInput {
    crate::JobInput::Media { role: "images".into(), mime: "image/png".into(), bytes: Arc::from(png(n, n)) }
}
fn training(images: u32) -> ProviderJob {
    let mut j = job(FLUX_LORA_TRAINER);
    j.inputs = (1..=images).map(image).collect();
    j.params.insert("steps".into(), json!(1500));
    j.params.insert("trigger_word".into(), json!("inkwash style"));
    j
}
fn style(provider: &str, trainer: &str, family: &str, weights: &str) -> Vec<u8> {
    let style = nodecraft_media::style::Style {
        provider: provider.into(),
        trainer: trainer.into(),
        family: family.into(),
        trigger: "inkwash style".into(),
        weights: weights.into(),
    };
    nodecraft_media::style::encode(&style).unwrap()
}
fn styled(style: Vec<u8>) -> ProviderJob {
    let mut j = job(FLUX_LORA);
    j.inputs.push(crate::JobInput::Media { role: "style".into(), mime: nodecraft_media::style::MIME.into(), bytes: Arc::from(style) });
    j.params.insert("lora_scale".into(), json!(0.8));
    j
}
fn lora(j: &ProviderJob) -> Result<serde_json::Value, ProviderError> {
    request::prepare(j, Limits::HARD).map(|(_, body)| serde_json::from_slice(&body).unwrap())
}
fn trained(weights: &str) -> serde_json::Value {
    json!({
        "diffusers_lora_file": {"url": weights, "content_type": "application/octet-stream", "file_name": "pytorch_lora_weights.safetensors", "file_size": 89_745_224},
        "config_file": {"url": "https://v3.fal.media/files/lion/config.json", "content_type": "application/octet-stream", "file_name": "config.json", "file_size": 1024},
    })
}

#[test]
fn a_training_sends_its_images_as_one_zip_and_keeps_a_descriptor_not_the_weights() {
    let t = Transport::new(vec![receipt(FLUX_LORA_TRAINER), json!({"status":"COMPLETED"}), trained(WEIGHTS)]);
    let f = fal(&t);
    let j = training(4);
    let Submission::Queued(h) = block_on(f.submit(&j)).unwrap() else { panic!() };
    assert_eq!(h.data["trigger"], "inkwash style");
    assert_eq!(t.requests.lock().unwrap()[0].url, format!("{ORIGIN}/{FLUX_LORA_TRAINER}"));
    let body = t.body(0);
    assert_eq!((body["steps"].clone(), body["trigger_word"].clone()), (json!(1500), json!("inkwash style")));
    assert_eq!((body["is_style"].clone(), body["create_masks"].clone()), (json!(true), json!(false)));
    let uri = body["images_data_url"].as_str().unwrap();
    let zip = base64::engine::general_purpose::STANDARD.decode(uri.strip_prefix("data:application/zip;base64,").unwrap()).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    assert_eq!(archive.len(), 4);
    for (n, input) in j.inputs.iter().enumerate() {
        let crate::JobInput::Media { bytes, .. } = input else { panic!() };
        let mut file = archive.by_name(&format!("{n:02}.png")).unwrap();
        let mut read = Vec::new();
        file.read_to_end(&mut read).unwrap();
        assert_eq!(&read[..], &bytes[..]);
    }
    // Resumed from its saved handle: the trigger survives, and the weights are never fetched.
    let restored: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    let JobProgress::Done(done) = block_on(f.poll(&restored)).unwrap() else { panic!() };
    let [Output::Bytes { mime, bytes }] = &done.outputs[..] else { panic!("{done:?}") };
    assert_eq!(mime, nodecraft_media::style::MIME);
    let style = nodecraft_media::style::decode(bytes).unwrap();
    assert_eq!((style.provider.as_str(), style.trainer.as_str(), style.family.as_str()), ("fal", FLUX_LORA_TRAINER, STYLE_FAMILY));
    assert_eq!((style.trigger.as_str(), style.weights.as_str()), ("inkwash style", WEIGHTS));
    assert_eq!(t.requests.lock().unwrap().len(), 3);
}

#[test]
fn a_training_refuses_bad_datasets_and_keyed_or_foreign_weights() {
    for j in [
        training(3),
        training(13),
        ProviderJob {
            inputs: vec![
                image(1),
                image(2),
                image(3),
                crate::JobInput::Media { role: "images".into(), mime: "image/png".into(), bytes: Arc::from(&b"not an image"[..]) },
            ],
            ..training(4)
        },
        ProviderJob {
            inputs: vec![image(1), image(2), image(3), crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png(4, 4)) }],
            ..training(4)
        },
    ] {
        assert!(request::prepare(&j, Limits::HARD).is_err(), "{} inputs", j.inputs.len());
    }
    for trigger in [json!("a\nb"), json!("x".repeat(65)), json!(7)] {
        let mut j = training(4);
        j.params.insert("trigger_word".into(), trigger.clone());
        assert!(request::prepare(&j, Limits::HARD).is_err() || request::trigger(&j).is_err(), "{trigger}");
    }
    // Spaces around a typed trigger are dropped.
    let mut j = training(4);
    j.params.insert("trigger_word".into(), json!(" inkwash style "));
    assert_eq!(request::trigger(&j).unwrap(), "inkwash style");
    // The documented most: twelve images of the per-image cap still pack within the dataset cap.
    let big = vec![0u8; Limits::HARD.input_bytes];
    let raw: Vec<(&str, &[u8])> = (0..12).map(|_| ("image/png", &big[..])).collect();
    let packed = request::dataset(&raw, Limits::HARD.dataset_bytes).unwrap();
    assert!(packed.len() <= Limits::HARD.dataset_bytes);
    let mut j = training(4);
    j.params.insert("steps".into(), json!(5001));
    assert!(request::prepare(&j, Limits::HARD).is_err());
    // A dataset over its byte cap is refused before any image is decoded.
    assert!(request::prepare(&training(4), Limits { dataset_bytes: 64, ..Limits::HARD }).is_err());
    for weights in ["https://v3.fal.media/files/w.safetensors?token=secret", "https://files.example/w.safetensors"] {
        let t = Transport::new(vec![receipt(FLUX_LORA_TRAINER), json!({"status":"COMPLETED"}), trained(weights)]);
        let f = fal(&t);
        let Submission::Queued(h) = block_on(f.submit(&training(4))).unwrap() else { panic!() };
        let error = block_on(f.poll(&h)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "{weights}");
        assert!(!error.message.contains("secret"));
    }
}

#[test]
fn a_training_handle_keeps_its_trigger_and_no_other_handle_has_one() {
    let t = Transport::new(vec![receipt(FLUX_LORA_TRAINER)]);
    let Submission::Queued(h) = block_on(fal(&t).submit(&training(4))).unwrap() else { panic!() };
    assert!(Record::read(&h, &Secret::new(KEY).unwrap()).is_ok());
    let mut missing = h.clone();
    missing.data.as_object_mut().unwrap().remove("trigger");
    let mut odd = h.clone();
    odd.data["trigger"] = json!("two\nlines");
    let mut flux = super::tests::handle(FLUX);
    flux.data["trigger"] = json!("inkwash style");
    for h in [missing, odd, flux] {
        assert!(Record::read(&h, &Secret::new(KEY).unwrap()).is_err(), "{}", h.data);
    }
}

#[test]
fn a_styled_image_sends_fal_s_own_weights_with_its_trigger_and_downloads_like_flux() {
    let body = lora(&styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS))).unwrap();
    assert_eq!(body["loras"], json!([{"path": WEIGHTS, "scale": 0.8}]));
    assert_eq!(body["prompt"], "inkwash style, A blue circle");
    assert_eq!((body["num_inference_steps"].clone(), body["guidance_scale"].clone()), (json!(28), json!(3.5)));
    // A prompt that already calls the style up is left alone.
    let mut j = styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS));
    j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "a heron, inkwash style".into() };
    assert_eq!(lora(&j).unwrap()["prompt"], "a heron, inkwash style");

    let t = Transport::new(vec![
        receipt(FLUX_LORA),
        json!({"status":"COMPLETED"}),
        json!({"images":[{"url":"https://v3.fal.media/files/out.png","width":2,"height":2}]}),
    ]);
    t.push(png(2, 2));
    let f = fal(&t);
    let Submission::Queued(h) = block_on(f.submit(&styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS)))).unwrap() else { panic!() };
    assert!(h.data.get("trigger").is_none());
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert_eq!(done, Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png(2, 2)) }]));
}

#[test]
fn a_styled_image_refuses_other_providers_families_and_links_before_sending() {
    for (bytes, why) in [
        (style("mock", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS), "another provider"),
        (style("fal", "fal-ai/other-trainer", STYLE_FAMILY, WEIGHTS), "another trainer"),
        (style("fal", FLUX_LORA_TRAINER, "sdxl-1", WEIGHTS), "another family"),
        (style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, "https://files.example/w.safetensors"), "weights off fal"),
        (b"{\"nodecraft_style\":1,".to_vec(), "damaged"),
    ] {
        assert_eq!(lora(&styled(bytes)).unwrap_err().kind, ErrorKind::UnsupportedInput, "{why}");
    }
    assert!(lora(&job(FLUX_LORA)).is_err(), "a style is required");
    let mut j = styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS));
    j.params.insert("lora_scale".into(), json!(2.5));
    assert!(lora(&j).is_err());
    // Plain FLUX takes no style.
    let mut j = styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS));
    j.model = FLUX.into();
    j.params.clear();
    assert!(lora(&j).is_err());
}

#[test]
fn the_catalog_entries_train_and_take_the_family_the_adapter_checks() {
    use nodecraft_types::{ParamValues, Value};
    let c = nodecraft_catalog::Catalog::builtin().unwrap();
    for (id, wire) in [("fal.flux-lora-trainer", FLUX_LORA_TRAINER), ("fal.flux-lora", FLUX_LORA)] {
        let model = c.get(id).unwrap();
        assert_eq!((model.provider_model.as_str(), model.style_family.as_deref()), (wire, Some(STYLE_FAMILY)));
        let mut j = if wire == FLUX_LORA { styled(style("fal", FLUX_LORA_TRAINER, STYLE_FAMILY, WEIGHTS)) } else { training(4) };
        j.params.clear();
        for (name, value) in model.params.resolve(&ParamValues::new(), false).unwrap().visible_values() {
            let v = match value {
                Value::Unsigned(n) => json!(n.0),
                Value::Number(n) => json!(n.get()),
                Value::Text(s) => json!(s),
                _ => panic!("scalar model values"),
            };
            j.params.insert(name.clone(), v);
        }
        assert!(request::prepare(&j, Limits::HARD).is_ok(), "{id} defaults");
    }
    let trainer = c.get("fal.flux-lora-trainer").unwrap();
    let images = trainer.modes.iter().flat_map(|m| &m.inputs).find(|i| i.role == "images").unwrap();
    assert_eq!((images.min as usize, images.max as usize), (*request::DATASET.start(), *request::DATASET.end()));
    // 1000 steps is 2 USD.
    assert_eq!(trainer.quote(&ParamValues::new()).unwrap().map(|m| m.micros), Some(2_000_000));
}
