//! `plugin try` (#866) on the example text action and a provider template, with scripted
//! replies: no key, no network.
use super::*;
use nodecraft_plugin_store::archive;

fn shout() -> Vec<u8> {
    // Packed as `plugin pack` packs it: the manifest made canonical.
    let files = [
        ("LICENSE", include_bytes!("../../../../examples/plugins/shout/LICENSE").as_slice()),
        ("actions/shout.json", include_bytes!("../../../../examples/plugins/shout/actions/shout.json").as_slice()),
        ("manifest.json", include_bytes!("../../../../examples/plugins/shout/manifest.json").as_slice()),
    ];
    nodecraft_plugin_store::author::pack(files.iter().map(|(p, b)| ((*p).to_owned(), b.to_vec())).collect(), None).unwrap().bytes
}

fn acme() -> Vec<u8> {
    let manifest = json!({
        "manifest_version": 1, "package_id": "io.example.acme", "version": "1.0.0", "license": "MIT",
        "host_protocol": {"major": 1, "min_minor": 0, "max_minor": 0}, "descriptor_contract": {"major": 1, "minor": 0},
        "runtime": {"kind": "provider_operations", "schema": 1}, "dependencies": [],
        "capabilities": {"required": ["network", "secret"], "optional": []},
        "registrations": [{"id": "acme", "kind": "provider", "descriptor": "providers/acme.json"}]
    })
    .to_string();
    let template = json!({
        "schema": 1, "label": "Acme", "origin": "https://api.acme.example", "output_origins": ["https://cdn.acme.example"], "credential": "bearer",
        "models": [{"id": "acme/v1", "price": {"currency": "USD", "micros": "40000"}}],
        "submit": {"method": "POST", "path": "/v1/{model}", "body": {"prompt": "{text:prompt}"}, "outputs": [{"at": "/url", "kind": "url", "mime": "image/png"}]},
        "limits": {"response_bytes": 65536, "timeout_ms": 30000, "concurrency": 1}
    })
    .to_string();
    archive::build(&[("LICENSE", b"MIT\n".as_slice()), ("manifest.json", manifest.as_bytes()), ("providers/acme.json", template.as_bytes())], None).unwrap()
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}

fn run(bytes: &[u8], what: &Try) -> Result<Value, Error> {
    futures::executor::block_on(try_package(bytes, what))
}

#[test]
fn the_example_text_action_runs_on_its_inputs() {
    let what = Try { registration: "shout".into(), inputs: [("words".to_owned(), "hello there".to_owned())].into(), ..Try::default() };
    let report = run(&shout(), &what).unwrap();
    assert_eq!(report["kind"], "action");
    assert_eq!(report["outputs"], json!({"out": "HELLO THERE"}));
    let missing = run(&shout(), &Try { registration: "shout".into(), ..Try::default() }).unwrap_err();
    assert!(missing.message.contains("the action failed"), "{missing:?}");
    assert!(run(&shout(), &Try { registration: "whisper".into(), ..Try::default() }).is_err(), "an unknown registration");
}

#[test]
fn a_provider_template_is_sent_by_the_broker_over_the_scripted_replies() {
    let what = Try {
        registration: "acme".into(),
        inputs: [("prompt".to_owned(), "A lighthouse".to_owned())].into(),
        replies: vec![
            Reply { status: 200, headers: vec![], body: br#"{"url":"https://cdn.acme.example/out/1.png"}"#.to_vec() },
            Reply { status: 200, headers: vec![], body: png() },
        ],
        ..Try::default()
    };
    let report = run(&acme(), &what).unwrap();
    assert_eq!(report["kind"], "provider");
    let requests = report["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 2, "the submit, then the output's download: {report}");
    assert_eq!(requests[1]["url"], "https://cdn.acme.example/out/1.png");
    assert_eq!(requests[1]["headers"], json!([]), "the download goes without the credential");
    assert_eq!(requests[0]["url"], "https://api.acme.example/v1/acme/v1");
    assert_eq!(serde_json::from_str::<Value>(requests[0]["body"].as_str().unwrap()).unwrap(), json!({"prompt": "A lighthouse"}));
    assert!(requests[0]["headers"].as_array().unwrap().iter().any(|h| h.as_str().is_some_and(|h| h.eq_ignore_ascii_case("authorization"))));
    assert!(!report.to_string().contains(FIXTURE_KEY), "the fixture credential's value is never shown");
    assert_eq!(report["outcome"]["outputs"], json!([{"mime": "image/png", "bytes": png().len()}]), "{report}");

    // With no reply left, the request fails as the network would, and the run says it may have
    // been billed: the broker's own rule for a lost reply to the paid request.
    let what = Try { replies: vec![], ..what };
    let report = run(&acme(), &what).unwrap();
    assert!(report["outcome"]["error"].as_str().is_some_and(|e| e.contains("no scripted reply")), "{report}");
    assert_eq!(report["outcome"]["possibly_billed"], true);
}

fn palette() -> Vec<u8> {
    let files = [
        ("LICENSE", include_bytes!("../../../../examples/plugins/palette/LICENSE").as_slice()),
        ("data/warm.json", include_bytes!("../../../../examples/plugins/palette/data/warm.json").as_slice()),
        ("manifest.json", include_bytes!("../../../../examples/plugins/palette/manifest.json").as_slice()),
    ];
    nodecraft_plugin_store::author::pack(files.iter().map(|(p, b)| ((*p).to_owned(), b.to_vec())).collect(), None).unwrap().bytes
}

#[test]
fn what_cant_be_tried_is_refused() {
    // A data package runs nothing.
    let data = run(&palette(), &Try { registration: "warm".into(), ..Try::default() }).unwrap_err();
    assert!(data.message.contains("runs nothing"), "{data:?}");
    // A text action takes no model, parameters or replies.
    let shouted = Try { registration: "shout".into(), inputs: [("words".to_owned(), "hi".to_owned())].into(), model: Some("x".into()), ..Try::default() };
    assert!(run(&shout(), &shouted).unwrap_err().message.contains("only its inputs"));
    // Too many scripted replies.
    let what = Try { registration: "acme".into(), replies: vec![Reply { status: 200, headers: vec![], body: vec![] }; MAX_REPLIES + 1], ..Try::default() };
    assert!(run(&acme(), &what).is_err());
    // A package install would refuse (not a plugin at all) is refused the same way.
    assert!(run(b"not a package", &Try { registration: "acme".into(), ..Try::default() }).is_err());
}

/// The service refusing the request shows as the run's error, with what was sent.
#[test]
fn a_refused_request_is_the_error() {
    let what = Try {
        registration: "acme".into(),
        inputs: [("prompt".to_owned(), "A lighthouse".to_owned())].into(),
        replies: vec![Reply { status: 401, headers: vec![], body: br#"{"error":"bad key"}"#.to_vec() }],
        ..Try::default()
    };
    let report = run(&acme(), &what).unwrap();
    assert_eq!(report["requests"][0]["method"], "POST");
    assert!(report["outcome"]["error"].as_str().is_some_and(|e| e.contains("401")), "{report}");
}
