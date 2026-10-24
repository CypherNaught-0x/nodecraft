use serde_json::{Value, json};

use super::template::{MAX_TEMPLATE_BYTES, SubmitReply};
use super::*;
use crate::{ErrorKind, HttpMethod, JobHandle, JobInput, JobProgress, Output, ProviderJob, Submission};
use nodecraft_types::Modality;

fn queued() -> Value {
    json!({
        "schema": 1,
        "label": "Acme Images",
        "origin": "https://api.acme.example",
        "output_origins": ["https://cdn.acme.example"],
        "credential": "bearer",
        "idempotency": "idempotency-key",
        "models": [{"id": "acme/v1", "price": {"currency": "USD", "micros": "40000"}}],
        "submit": {
            "method": "POST",
            "path": "/v1/models/{model}/jobs",
            "body": {"model": "{model}", "prompt": "{text:prompt}", "options": {"seed": "{param:seed}", "steps": "{param:steps}"}, "tags": ["nodecraft"]},
            "handle": "/id"
        },
        "poll": {
            "method": "GET",
            "path": "/v1/jobs/{job}",
            "status": "/status",
            "states": {"queued": ["queued"], "running": ["running"], "done": ["succeeded"], "failed": ["failed"]},
            "outputs": [{"at": "/output/images", "kind": "url", "mime": "image/png"}],
            "error": "/error/message",
            "interval_ms": 2000
        },
        "cancel": {"method": "POST", "path": "/v1/jobs/{job}/cancel"},
        "limits": {"response_bytes": 65536, "timeout_ms": 30000, "concurrency": 2}
    })
}

fn decode(value: &Value) -> Result<Template, TemplateError> {
    Template::decode(value.to_string().as_bytes())
}

fn template() -> Template {
    decode(&queued()).unwrap()
}

/// `queued()` with `value` at the JSON pointer `at` (a missing last member is added).
fn with(at: &str, value: Value) -> Value {
    let mut t = queued();
    let (parent, key) = at.rsplit_once('/').unwrap();
    match t.pointer_mut(parent).unwrap() {
        Value::Object(map) => {
            map.insert(key.to_owned(), value);
        }
        Value::Array(items) => items[key.parse::<usize>().unwrap()] = value,
        _ => panic!("{at}"),
    }
    t
}

fn without(at: &str) -> Value {
    let mut t = queued();
    let (parent, key) = at.rsplit_once('/').unwrap();
    t.pointer_mut(parent).unwrap().as_object_mut().unwrap().remove(key);
    t
}

fn refused(value: Value, at: &str) {
    match decode(&value) {
        Err(e) => assert!(e.at.starts_with(at), "{at}: refused at {} ({})", e.at, e.message),
        Ok(_) => panic!("{at}: accepted {value}"),
    }
}

fn job() -> ProviderJob {
    let mut params = serde_json::Map::new();
    params.insert("seed".into(), json!(7));
    ProviderJob {
        model: "acme/v1".into(),
        params,
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "a red fox".into() }],
        idempotency_key: "nc-abc123".into(),
    }
}

#[test]
fn a_template_decodes_with_everything_the_host_needs() {
    let t = template();
    assert_eq!(t.label, "Acme Images");
    assert_eq!(t.origin.as_str(), "https://api.acme.example");
    assert_eq!(t.origins().map(Origin::as_str).collect::<Vec<_>>(), ["https://api.acme.example", "https://cdn.acme.example"]);
    assert_eq!(t.credential, Some(Credential::Bearer));
    assert_eq!(t.model("acme/v1").unwrap().price.micros, 40_000);
    assert!(matches!(t.submit.reply, SubmitReply::Queued { .. }));
    assert_eq!(t.poll.as_ref().unwrap().interval_ms, 2000);
}

#[test]
fn origins_are_https_public_dns_names_and_nothing_else() {
    for good in ["https://api.example.com", "https://a-b.example.co.uk", "https://xn--bcher-kva.example"] {
        assert!(Origin::parse(good).is_ok(), "{good}");
    }
    for bad in [
        "http://api.example.com",
        "https://api.example.com/",
        "https://api.example.com/v1",
        "https://api.example.com:8443",
        "https://api.example.com:443",
        "https://user:pw@api.example.com",
        "https://api.example.com?x=1",
        "https://API.example.com",
        "https://127.0.0.1",
        "https://10.0.0.8",
        "https://0x7f.1",
        "https://[::1]",
        "https://localhost",
        "https://printer.local",
        "https://db.internal",
        "https://router.home.arpa",
        "https://metadata.google.internal",
        "https://example",
        "https://ex ample.com",
        "file:///etc/passwd",
        "",
    ] {
        assert!(Origin::parse(bad).is_err(), "{bad}");
    }
    refused(with("/origin", json!("https://169.254.169.254")), "/origin");
    refused(with("/output_origins", json!(["https://api.acme.example"])), "/output_origins/0");
    refused(with("/output_origins", json!(["http://cdn.acme.example"])), "/output_origins/0");
}

#[test]
fn a_template_names_no_header_of_its_own() {
    // The credential's placement is from the host's list; any other field is refused.
    refused(with("/credential", json!("cookie")), "");
    refused(with("/credential", json!({"header": "x-forward-to", "value": "{secret}"})), "");
    refused(with("/headers", json!({"x-api-key": "{secret}"})), "");
    refused(with("/idempotency", json!("x-custom")), "");
    refused(with("/submit/headers", json!({"authorization": "Bearer x"})), "");
    refused(with("/submit/url", json!("https://evil.example/steal")), "");
}

#[test]
fn a_model_is_never_free_and_its_price_is_exact() {
    refused(with("/models/0/price", json!({"currency": "USD", "micros": "0"})), "/models/0/price");
    refused(with("/models/0/price", json!({"currency": "USD", "micros": 0.04})), "");
    refused(with("/models/0/price", json!({"currency": "usd", "micros": "40000"})), "");
    refused(with("/models/0/price", json!({"currency": "USD", "micros": "-1"})), "");
    refused(with("/models", json!([])), "/models");
    let twice = json!([{"id": "a", "price": {"currency": "USD", "micros": "1"}}, {"id": "a", "price": {"currency": "USD", "micros": "1"}}]);
    refused(with("/models", twice), "/models/1/id");
    for bad in ["", "../v1", "a//b", "a/./b", "a?b", "a b", "a%2fb"] {
        refused(with("/models/0/id", json!(bad)), "/models/0/id");
    }
}

#[test]
fn paths_take_only_checked_placeholders() {
    for bad in ["v1/jobs", "/v1/../admin", "/v1//jobs", "/v1/jobs?x=1", "/v1/{job}", "/v1/{secret}", "/v1/pre{model}", "/v1/%2e%2e", "/v1/a#b"] {
        refused(with("/submit/path", json!(bad)), "/submit/path");
    }
    refused(with("/poll/path", json!("/v1/jobs")), "/poll/path");
    refused(with("/cancel/path", json!("/v1/cancel")), "/cancel/path");
    refused(with("/submit/method", json!("GET")), "/submit/method");
    refused(with("/poll/method", json!("POST")), "/poll/method");
    refused(with("/cancel/method", json!("PUT")), "");
}

#[test]
fn bodies_take_only_known_placeholders_within_bounds() {
    refused(with("/submit/body", json!({"key": "{secret}"})), "/submit/body/key");
    refused(with("/submit/body", json!({"key": "{param:Bad-Name}"})), "/submit/body/key");
    refused(with("/submit/body", json!({"key": "{env:HOME}"})), "/submit/body/key");
    let mut deep = json!("x");
    for _ in 0..20 {
        deep = json!([deep]);
    }
    refused(with("/submit/body", deep), "/submit/body");
    let many: Vec<Value> = (0..65).map(|_| json!("{model}")).collect();
    refused(with("/submit/body", Value::Array(many)), "/submit/body");
}

#[test]
fn replies_and_limits_are_bounded() {
    refused(without("/poll"), "/poll");
    refused(with("/submit/outputs", json!([{"at": "/x", "kind": "text"}])), "/submit");
    refused(with("/limits/response_bytes", json!(64 * 1024 * 1024)), "/limits/response_bytes");
    refused(with("/limits/timeout_ms", json!(3_600_000)), "/limits/timeout_ms");
    refused(with("/limits/concurrency", json!(64)), "/limits/concurrency");
    refused(with("/limits/concurrency", json!(0)), "/limits/concurrency");
    refused(with("/poll/interval_ms", json!(10)), "/poll/interval_ms");
    refused(with("/poll/states/failed", json!(["succeeded"])), "/poll/states/failed/0");
    refused(with("/poll/states/failed", json!([])), "/poll/states");
    refused(with("/poll/outputs", json!([])), "/poll/outputs");
    refused(with("/poll/outputs/0/at", json!("output")), "/poll/outputs/0/at");
    refused(with("/poll/outputs/0/mime", json!("Image/PNG")), "/poll/outputs/0/mime");
    refused(with("/schema", json!(2)), "/schema");
    refused(with("/label", json!(" ")), "/label");
    let big = format!("{{\"label\": \"{}\"}}", "x".repeat(MAX_TEMPLATE_BYTES));
    assert!(Template::decode(big.as_bytes()).is_err());
    assert!(Template::decode(b"\xff\xfe").is_err());
}

#[test]
fn a_decode_error_does_not_echo_the_template() {
    let e = Template::decode(br#"{"schema": 1, "label": "sk-live-1234567890", "zzz": true}"#).unwrap_err();
    assert!(!e.to_string().contains("sk-live") && !e.to_string().contains("zzz"), "{e}");
    // A key pasted where a placement goes is quoted by serde's unknown-variant message.
    let e = decode(&with("/credential", json!("sk-live-1234567890\u{1b}[2J"))).unwrap_err();
    assert!(!e.to_string().contains("sk-live") && !e.to_string().contains('\u{1b}'), "{e}");
    let e = decode(&with("/sk-live-1234567890", json!(1))).unwrap_err();
    assert!(!e.to_string().contains("sk-live"), "{e}");
}

#[test]
fn a_submit_fills_only_its_placeholders_on_the_templates_origin() {
    let t = template();
    let request = plan_submit(&t, &job()).unwrap();
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.url, "https://api.acme.example/v1/models/acme/v1/jobs");
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    // `steps` isn't set by the job, so its member is left out; literal values stay.
    assert_eq!(body, json!({"model": "acme/v1", "prompt": "a red fox", "options": {"seed": 7}, "tags": ["nodecraft"]}));
    assert!(request.headers.iter().any(|(n, v)| n == "idempotency-key" && v == "nc-abc123"));
    assert!(!request.headers.iter().any(|(n, _)| n == "authorization"), "planned requests carry no credential");
    assert_eq!(request.timeout_ms, 30_000);
    assert_eq!(request.max_response_bytes, 65_536);

    let authorized = authorize(&t, request, "sk-test").unwrap();
    assert!(authorized.headers.iter().any(|(n, v)| n == "authorization" && v == "Bearer sk-test"));
    assert!(authorized.secret_headers.iter().any(|n| n == "authorization"));
    assert!(!format!("{authorized:?}").contains("sk-test"));
}

#[test]
fn a_job_cannot_steer_the_request_elsewhere() {
    let t = template();
    let mut other = job();
    other.model = "https://evil.example/x".into();
    assert_eq!(plan_submit(&t, &other).unwrap_err().kind, ErrorKind::UnsupportedInput);

    // A param or prompt that looks like a URL or a placeholder is only a value in the body.
    let mut sneaky = job();
    sneaky.params.insert("seed".into(), json!("{model}"));
    sneaky.inputs = vec![JobInput::Text { role: "prompt".into(), text: "https://evil.example/{job}".into() }];
    let request = plan_submit(&t, &sneaky).unwrap();
    assert_eq!(request.url, "https://api.acme.example/v1/models/acme/v1/jobs");
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["options"]["seed"], "{model}");
    assert_eq!(body["prompt"], "https://evil.example/{job}");

    let mut media = job();
    media.inputs.push(JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: vec![1u8].into() });
    assert_eq!(plan_submit(&t, &media).unwrap_err().kind, ErrorKind::UnsupportedInput);

    let mut twice = job();
    twice.inputs.push(JobInput::Text { role: "prompt".into(), text: "again".into() });
    assert_eq!(plan_submit(&t, &twice).unwrap_err().kind, ErrorKind::UnsupportedInput);

    let mut keyless = job();
    keyless.idempotency_key = "bad key\n".into();
    assert_eq!(plan_submit(&t, &keyless).unwrap_err().kind, ErrorKind::UnsupportedInput);
}

#[test]
fn a_reply_is_read_only_at_its_fields() {
    let t = template();
    let reply = json!({"id": "job_42", "debug": {"echo": "sk-test"}}).to_string();
    let Submission::Queued(handle) = read_submit(&t, "acme/v1", reply.as_bytes()).unwrap() else { panic!("queued") };
    assert_eq!(handle, JobHandle { remote_id: "job_42".into(), data: json!({"model": "acme/v1"}) });
    assert_eq!(plan_poll(&t, &handle).unwrap().url, "https://api.acme.example/v1/jobs/job_42");
    let cancel = plan_cancel(&t, &handle).unwrap().unwrap();
    assert_eq!((cancel.method, cancel.url.as_str()), (HttpMethod::Post, "https://api.acme.example/v1/jobs/job_42/cancel"));

    for id in [json!("../admin"), json!("a/b"), json!("a?b"), json!(""), json!(42), json!("x".repeat(200))] {
        let reply = json!({"id": id}).to_string();
        assert_eq!(read_submit(&t, "acme/v1", reply.as_bytes()).unwrap_err().kind, ErrorKind::Malformed, "{id}");
    }
    let forged = JobHandle { remote_id: "job_42".into(), data: json!({"model": "elsewhere"}) };
    assert_eq!(plan_poll(&t, &forged).unwrap_err().kind, ErrorKind::Malformed);
    let traversal = JobHandle { remote_id: "..".into(), data: json!({"model": "acme/v1"}) };
    assert_eq!(plan_poll(&t, &traversal).unwrap_err().kind, ErrorKind::Malformed);
    assert!(read_submit(&t, "acme/v1", b"not json").is_err());
    assert!(read_submit(&t, "acme/v1", &vec![b' '; 70_000]).is_err());
}

#[test]
fn a_poll_maps_states_and_keeps_outputs_on_its_origins() {
    let t = template();
    let read = |v: Value| read_poll(&t, v.to_string().as_bytes(), "");
    assert_eq!(read(json!({"status": "queued"})).unwrap(), JobProgress::Queued { position: None });
    assert!(matches!(read(json!({"status": "something new"})).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) =
        read(json!({"status": "succeeded", "output": {"images": ["https://cdn.acme.example/a.png", "https://api.acme.example/b.png"]}})).unwrap()
    else {
        panic!("done")
    };
    assert_eq!(
        done.outputs,
        vec![
            Output::Url { url: "https://cdn.acme.example/a.png".into(), mime: Some("image/png".into()) },
            Output::Url { url: "https://api.acme.example/b.png".into(), mime: Some("image/png".into()) },
        ]
    );
    for link in [
        "https://evil.example/a.png",
        "http://cdn.acme.example/a.png",
        "https://cdn.acme.example.evil.example/a.png",
        "https://127.0.0.1/a.png",
        "file:///etc/passwd",
    ] {
        let e = read(json!({"status": "succeeded", "output": {"images": [link]}})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed, "{link}");
    }
    assert!(read(json!({"status": "succeeded"})).is_err(), "no outputs");
    let many: Vec<String> = (0..17).map(|i| format!("https://cdn.acme.example/{i}.png")).collect();
    assert!(read(json!({"status": "succeeded", "output": {"images": many}})).is_err());
    let JobProgress::Failed(e) = read(json!({"status": "failed", "error": {"message": format!("bad\u{7}{}", "x".repeat(400))}})).unwrap() else {
        panic!("failed")
    };
    assert_eq!(e.kind, ErrorKind::JobFailed);
    assert!(e.message.len() <= 300 && !e.message.contains('\u{7}'));
    assert!(read(json!({"state": "succeeded"})).is_err(), "no status");
}

#[test]
fn a_synchronous_template_finishes_in_its_reply() {
    let mut v = without("/poll");
    v.as_object_mut().unwrap().remove("cancel");
    let submit = v.pointer_mut("/submit").unwrap().as_object_mut().unwrap();
    submit.remove("handle");
    submit.insert("outputs".into(), json!([{"at": "/text", "kind": "text"}]));
    let t = decode(&v).unwrap();
    let Submission::Finished(done) = read_submit(&t, "acme/v1", br#"{"text": "hello"}"#).unwrap() else { panic!("finished") };
    assert_eq!(done.outputs, vec![Output::Text("hello".into())]);
    let handle = JobHandle { remote_id: "j".into(), data: json!({"model": "acme/v1"}) };
    assert!(plan_poll(&t, &handle).is_err());
    assert!(plan_cancel(&t, &handle).unwrap().is_none());

    let mut cancel_only = v.clone();
    cancel_only.as_object_mut().unwrap().insert("cancel".into(), json!({"method": "DELETE", "path": "/v1/{job}"}));
    refused(cancel_only, "/cancel");
}

#[test]
fn the_credential_goes_only_to_the_api_origin() {
    let t = template();
    for url in [
        "https://cdn.acme.example/a.png",
        "https://evil.example/v1/jobs",
        "http://api.acme.example/v1/jobs",
        "https://api.acme.example:8443/v1/jobs",
        "https://u:p@api.acme.example/v1/jobs",
        "not a url",
    ] {
        let request = crate::HttpRequest::get(url);
        let e = authorize(&t, request, "sk-test").unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{url}");
        assert!(!e.message.contains("sk-test") && !e.message.contains(url), "{url}");
    }
    let handle = JobHandle { remote_id: "j1".into(), data: json!({"model": "acme/v1"}) };
    assert!(authorize(&t, plan_poll(&t, &handle).unwrap(), "sk-test").is_ok());
}

#[test]
fn each_credential_placement_is_a_fixed_header() {
    for (placement, name, value) in [
        ("bearer", "authorization", "Bearer sk-test"),
        ("key", "authorization", "Key sk-test"),
        ("x_api_key", "x-api-key", "sk-test"),
        ("api_key", "api-key", "sk-test"),
        ("x_goog_api_key", "x-goog-api-key", "sk-test"),
    ] {
        let t = decode(&with("/credential", json!(placement))).unwrap();
        let request = authorize(&t, plan_submit(&t, &job()).unwrap(), "sk-test").unwrap();
        assert!(request.headers.iter().any(|(n, v)| n == name && v == value), "{placement}");
        assert!(request.secret_headers.iter().any(|n| n == name), "{placement}");
    }
}

#[test]
fn an_output_link_carries_no_user_name() {
    let t = template();
    for link in ["https://u:p@cdn.acme.example/a.png", "https://u@cdn.acme.example/a.png"] {
        let e = read_poll(&t, json!({"status": "succeeded", "output": {"images": [link]}}).to_string().as_bytes(), "").unwrap_err();
        assert_eq!(e.kind, ErrorKind::Malformed, "{link}");
    }
}

#[test]
fn an_unauthenticated_template_sends_no_credential() {
    let t = decode(&without("/credential")).unwrap();
    let request = authorize(&t, plan_submit(&t, &job()).unwrap(), "sk-test").unwrap();
    assert!(!request.headers.iter().any(|(_, v)| v.contains("sk-test")));
}

mod adapter;

#[test]
fn a_model_says_what_it_makes_and_what_it_is_called() {
    let m = template().models.remove(0);
    assert_eq!((m.label.as_str(), m.output), ("acme/v1", Modality::Image), "links make an image unless the template says otherwise");
    let named =
        decode(&with("/models/0", json!({"id": "acme/v1", "label": " Acme V1 ", "output": "video", "price": {"currency": "USD", "micros": "1"}}))).unwrap();
    assert_eq!((named.models[0].label.as_str(), named.models[0].output), ("Acme V1", Modality::Video));
    for bad in [json!({"output": "style"}), json!({"output": "document"}), json!({"output": "text"}), json!({"label": ""}), json!({"label": "a\u{7}"})] {
        let mut model = json!({"id": "acme/v1", "price": {"currency": "USD", "micros": "1"}});
        model.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        refused(with("/models/0", model), "/models/0");
    }

    // A reply of text makes text, and only text.
    let mut v = without("/poll");
    v.as_object_mut().unwrap().remove("cancel");
    let submit = v.pointer_mut("/submit").unwrap().as_object_mut().unwrap();
    submit.remove("handle");
    submit.insert("outputs".into(), json!([{"at": "/text", "kind": "text"}]));
    assert_eq!(decode(&v).unwrap().models[0].output, Modality::Text);
    *v.pointer_mut("/models/0").unwrap() = json!({"id": "acme/v1", "output": "image", "price": {"currency": "USD", "micros": "1"}});
    refused(v, "/models/0/output");
}

#[test]
fn text_roles_are_the_body_text_placeholders() {
    assert_eq!(template().text_roles().into_iter().collect::<Vec<_>>(), ["prompt"]);
    let t = decode(&with("/submit/body", json!({"a": ["{text:negative}", {"b": "{text:prompt}"}], "c": "{param:seed}", "d": "{text:prompt}"}))).unwrap();
    assert_eq!(t.text_roles().into_iter().collect::<Vec<_>>(), ["negative", "prompt"]);
}
