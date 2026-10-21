use super::*;
use nodecraft_nodes::GenerateInput;
use nodecraft_types::{Integer, NodeId, Number};

fn routes() -> Vec<GenerateOutput> {
    vec![GenerateOutput { port: "image".into(), modality: Modality::Image }, GenerateOutput { port: "caption".into(), modality: Modality::Text }]
}

fn spec(inputs: Vec<GenerateInput>) -> JobSpec {
    let params = [
        ("steps".to_owned(), Value::Integer(Integer(20))),
        ("cfg".to_owned(), Value::Number(Number::new(4.5).unwrap())),
        ("hd".to_owned(), Value::Bool(true)),
        ("style".to_owned(), Value::Text("ink".into())),
    ]
    .into_iter()
    .collect();
    JobSpec::Generate {
        instance: None,
        node: NodeId::from_bytes([1; 16]),
        outputs: routes(),
        provider: "mock".into(),
        model: "mock-image".into(),
        params,
        inputs,
    }
}

#[test]
fn a_generate_job_becomes_a_provider_request() {
    let given = vec![
        GenerateInput { role: "prompt".into(), value: Value::Text("a fox".into()) },
        GenerateInput { role: "reference".into(), value: Value::List(vec![Value::Text("one".into()), Value::Text("two".into())]) },
    ];
    let (provider, job) = provider_job(&spec(given), "nodecraft:r:1").unwrap();
    assert_eq!(provider, "mock");
    assert_eq!(job.model, "mock-image");
    assert_eq!(job.idempotency_key, "nodecraft:r:1");
    assert_eq!(Json::Object(job.params), serde_json::json!({"steps": 20, "cfg": 4.5, "hd": true, "style": "ink"}));
    assert_eq!(job.inputs.len(), 3);
    assert_eq!(job.inputs[0], JobInput::Text { role: "prompt".into(), text: "a fox".into() });
    // Media inputs and source jobs are refused.
    let media = Value::Media(AssetRef { hash: Blake3::from_bytes([2; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
    assert_eq!(provider_job(&spec(vec![GenerateInput { role: "image".into(), value: media }]), "k").unwrap_err().kind, ErrorKind::Unsupported);
    let source = JobSpec::Source { node: NodeId::from_bytes([1; 16]), output: "out".into(), value: Value::Text("x".into()) };
    assert!(provider_job(&source, "k").is_err());
    // More inputs than a list can hold are refused while flattening.
    let many = Value::List((0..=MAX_VALUE_ITEMS).map(|n| Value::Text(n.to_string())).collect());
    assert!(provider_job(&spec(vec![GenerateInput { role: "prompt".into(), value: many }]), "k").is_err());
}

#[test]
fn results_are_sniffed_and_routed_by_modality() {
    let png: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\n0123456789"[..]);
    // The claimed type is ignored: the bytes are a PNG.
    let done = Completed::new(vec![Output::Text("a fox".into()), Output::Bytes { mime: "application/octet-stream".into(), bytes: Arc::clone(&png) }]);
    let got = produced(&routes(), &done).unwrap();
    assert_eq!(got.outputs[0], ("caption".to_owned(), Value::Text("a fox".into())));
    let (port, Value::Media(asset)) = &got.outputs[1] else { panic!("not media") };
    assert_eq!((port.as_str(), asset.mime.as_str()), ("image", "image/png"));
    assert_eq!(got.assets, vec![(asset.hash, png)]);
    // An HTML error page claiming to be a PNG is refused, not recorded as a caption; text
    // bytes the provider calls text go to the text port (without a UTF-8 BOM).
    let html = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"<html>error</html>"[..]) }]);
    assert!(produced(&routes(), &html).is_err());
    let text = Completed::new(vec![Output::Bytes { mime: "text/plain; charset=utf-8".into(), bytes: Arc::from(&b"\xEF\xBB\xBFhello"[..]) }]);
    assert_eq!(produced(&routes(), &text).unwrap().outputs, vec![("caption".to_owned(), Value::Text("hello".into()))]);
    let utf16 = Completed::new(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(&b"\xFF\xFEh\x00i\x00"[..]) }]);
    assert_eq!(produced(&routes(), &utf16).unwrap_err().kind, ErrorKind::Unsupported);
    let image_only = [GenerateOutput { port: "image".into(), modality: Modality::Image }];
    assert!(produced(&image_only, &text).is_err(), "no port takes text");
    // A paid call that returned nothing is a failure.
    assert!(produced(&routes(), &Completed::new(vec![])).is_err());
    // Unknown binary, audio on an image-only node, and URLs are refused.
    let binary = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"\x00\x01\x02\x03"[..]) }]);
    assert!(produced(&routes(), &binary).is_err());
    let wav = Completed::new(vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(&b"RIFF\x00\x00\x00\x00WAVEfmt "[..]) }]);
    assert!(produced(&image_only, &wav).is_err());
    assert!(produced(&routes(), &Completed::new(vec![Output::Url { url: "https://x".into(), mime: None }])).is_err());
}

#[test]
fn standalone_overdepth_parameters_are_rejected() {
    let mut job = spec(vec![]);
    if let JobSpec::Generate { params, .. } = &mut job {
        let value = (0..nodecraft_graph::MAX_VALUE_DEPTH + 1).fold(Value::Text("x".into()), |v, _| Value::List(vec![v]));
        params.insert("deep".into(), value);
    }
    assert!(provider_job(&job, "k").is_err(), "standalone job values require depth admission");
}
#[test]
fn standalone_empty_containers_count_toward_admission() {
    let value = Value::List(vec![Value::List(vec![]); MAX_VALUE_ITEMS + 1]);
    let job = spec(vec![GenerateInput { role: "prompt".into(), value }]);
    assert!(provider_job(&job, "k").is_err(), "empty containers must consume work admission");
}

fn bare(inputs: Vec<GenerateInput>) -> JobSpec {
    JobSpec::Generate {
        instance: None,
        node: NodeId::from_bytes([1; 16]),
        outputs: vec![],
        provider: "p".into(),
        model: "m".into(),
        params: Default::default(),
        inputs,
    }
}
fn param(value: Value) -> JobSpec {
    let mut job = bare(vec![]);
    if let JobSpec::Generate { params, .. } = &mut job {
        params.insert("a".into(), value);
    }
    job
}
fn no_request_copy(job: &JobSpec, limits: Limits) {
    let original = job.clone();
    let mut conversion = Conversion::default();
    assert!(request_with_limits(job, "k", limits, None, &mut conversion).is_err());
    assert_eq!(conversion, Conversion::default(), "refused request copied a prefix");
    assert_eq!(job, &original);
}
fn no_output_copy(done: &Completed, limits: Limits) {
    let original = done.clone();
    let mut conversion = Conversion::default();
    assert!(outputs_with_limits(&routes(), done, limits, &mut conversion).is_err());
    assert_eq!(conversion, Conversion::default(), "refused output copied/hashed a prefix");
    assert_eq!(done, &original);
}
fn png() -> Arc<[u8]> {
    Arc::from(b"\x89PNG\r\n\x1a\n0123456789".as_slice())
}
fn image(bytes: Arc<[u8]>) -> Output {
    Output::Bytes { mime: "image/png".into(), bytes }
}

#[test]
fn request_exact_representation_and_text_boundaries_copy_only_after_admission() {
    let job = param(Value::Text("b".into()));
    // Contract: provider/model/key 3*40, name40, Value128, text40 =328 upper-bound bytes.
    let mut conversion = Conversion::default();
    let (_, got) = request_with_limits(&job, "k", Limits { request: 328, text: 1, ..Limits::default() }, None, &mut conversion).unwrap();
    assert_eq!(got.params, serde_json::from_value::<Map<String, Json>>(serde_json::json!({"a":"b"})).unwrap());
    assert_eq!(conversion.json_nodes, 1);
    assert!(conversion.strings > 0 && conversion.text_bytes > 0);
    no_request_copy(&job, Limits { request: 327, ..Limits::default() });
    no_request_copy(&job, Limits { text: 0, ..Limits::default() });
    let unicode = param(Value::Text("é".into()));
    assert!(request_with_limits(&unicode, "k", Limits { text: 2, ..Limits::default() }, None, &mut Conversion::default()).is_ok());
    no_request_copy(&unicode, Limits { text: 1, ..Limits::default() });
}

#[test]
fn request_counts_all_roots_containers_depth_and_repeated_roles() {
    let nested = param(Value::List(vec![Value::List(vec![Value::Text("x".into())])]));
    assert!(request_with_limits(&nested, "k", Limits { depth: 2, items: 3, ..Limits::default() }, None, &mut Conversion::default()).is_ok());
    no_request_copy(&nested, Limits { depth: 1, ..Limits::default() });
    no_request_copy(&nested, Limits { items: 2, ..Limits::default() });
    let empty = param(Value::List(vec![Value::List(vec![]), Value::List(vec![])]));
    assert!(request_with_limits(&empty, "k", Limits { items: 3, ..Limits::default() }, None, &mut Conversion::default()).is_ok());
    no_request_copy(&empty, Limits { items: 2, ..Limits::default() });
    let mut roots = param(Value::Bool(false));
    if let JobSpec::Generate { inputs, .. } = &mut roots {
        inputs.push(GenerateInput { role: "r".into(), value: Value::List(vec![]) });
    }
    no_request_copy(&roots, Limits { items: 1, ..Limits::default() });
    let input = |role: &str| bare(vec![GenerateInput { role: role.into(), value: Value::List(vec![Value::Text("x".into()); 3]) }]);
    let mut conversion = Conversion::default();
    let (_, got) = request_with_limits(&input("r"), "k", Limits { request: 1200, ..Limits::default() }, None, &mut conversion).unwrap();
    assert_eq!(got.inputs.len(), 3);
    assert_eq!(conversion.input_records, 3);
    no_request_copy(&input("longrole"), Limits { request: 1200, ..Limits::default() });
    no_request_copy(&input("r"), Limits { inputs: 2, ..Limits::default() });
}

#[test]
fn request_metadata_and_late_unsupported_values_never_copy_prefixes() {
    for field in 0..5 {
        let mut job = param(Value::Text("accepted prefix".into()));
        if let JobSpec::Generate { provider, model, params, inputs, .. } = &mut job {
            match field {
                0 => *provider = "p".repeat(200),
                1 => *model = "m".repeat(200),
                2 => {
                    params.insert("n".repeat(200), Value::Bool(true));
                }
                3 => inputs.push(GenerateInput { role: "r".repeat(200), value: Value::List(vec![]) }),
                _ => inputs.push(GenerateInput { role: "r".into(), value: Value::Text("x".repeat(200)) }),
            }
        }
        no_request_copy(&job, Limits { request: 1000, ..Limits::default() });
    }
    let job = param(Value::Bool(true));
    let mut conversion = Conversion::default();
    assert!(request_with_limits(&job, &"k".repeat(200), Limits { request: 1000, ..Limits::default() }, None, &mut conversion).is_err());
    assert_eq!(conversion, Conversion::default());
    let asset = Value::Media(AssetRef { hash: Blake3::from_bytes([2; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() });
    let mut job = param(Value::Text("prefix".into()));
    if let JobSpec::Generate { params, .. } = &mut job {
        params.insert("z".into(), asset.clone());
    }
    no_request_copy(&job, Limits::default());
    let job = bare(vec![GenerateInput { role: "prompt".into(), value: Value::List(vec![Value::Text("prefix".into()), asset]) }]);
    no_request_copy(&job, Limits::default());
}

#[test]
fn text_output_exact_budget_and_aggregate_limits_are_pre_copy() {
    let route = [GenerateOutput { port: "t".into(), modality: Modality::Text }];
    let done = Completed::new(vec![Output::Text("ab".into())]);
    // Route104 + record128 + proof64+32 + port40 + text46 =414 logical bytes.
    let mut conversion = Conversion::default();
    let got = outputs_with_limits(&route, &done, Limits { outputs: 414, text: 2, raw_outputs: 2, ..Limits::default() }, &mut conversion).unwrap();
    assert_eq!(got.outputs, vec![("t".into(), Value::Text("ab".into()))]);
    assert_eq!(conversion.output_records, 1);
    assert_eq!(conversion.strings, 2);
    assert_eq!(conversion.text_bytes, 3);
    for limits in [Limits { outputs: 413, ..Limits::default() }, Limits { text: 1, ..Limits::default() }, Limits { raw_outputs: 1, ..Limits::default() }] {
        let mut conversion = Conversion::default();
        assert!(outputs_with_limits(&route, &done, limits, &mut conversion).is_err());
        assert_eq!(conversion, Conversion::default());
    }
    let done = Completed::new(vec![Output::Text("ab".into()), Output::Text("cd".into())]);
    assert!(outputs_with_limits(&routes(), &done, Limits { text: 4, ..Limits::default() }, &mut Conversion::default()).is_ok());
    no_output_copy(&done, Limits { text: 3, ..Limits::default() });
    no_output_copy(&done, Limits { items: 1, ..Limits::default() });
}

#[test]
fn mixed_binary_aggregate_counts_aliases_and_preserves_asset_arc_identity() {
    let bytes = png();
    let done = Completed::new(vec![Output::Text("ok".into()), image(Arc::clone(&bytes))]);
    let mut conversion = Conversion::default();
    let got =
        outputs_with_limits(&routes(), &done, Limits { inline: bytes.len(), raw_outputs: bytes.len() + 2, ..Limits::default() }, &mut conversion).unwrap();
    assert!(Arc::ptr_eq(&got.assets[0].1, &bytes));
    assert_eq!(conversion.asset_arcs, 1);
    assert_eq!(conversion.hash_bytes, bytes.len());
    assert_eq!(conversion.output_records, 2);
    assert!(conversion.strings > 0);
    no_output_copy(&done, Limits { raw_outputs: bytes.len() + 1, ..Limits::default() });
    no_output_copy(&done, Limits { inline: bytes.len() - 1, ..Limits::default() });
    let aliases = Completed::new(vec![image(Arc::clone(&bytes)), image(Arc::clone(&bytes))]);
    no_output_copy(&aliases, Limits { raw_outputs: 2 * bytes.len() - 1, ..Limits::default() });
    assert!(outputs_with_limits(&routes(), &aliases, Limits { raw_outputs: 2 * bytes.len(), ..Limits::default() }, &mut Conversion::default()).is_ok());
}

#[test]
fn late_semantic_output_failures_do_not_clone_hash_or_publish_valid_prefix() {
    for late in [
        Output::Url { url: "https://fixture.invalid/private-marker".into(), mime: None },
        Output::Bytes { mime: "image/png".into(), bytes: Arc::from(b"<html>error</html>".as_slice()) },
        Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(b"\xff\xfeh\0".as_slice()) },
        Output::Bytes { mime: "text/plain".into(), bytes: Arc::from([b'x'; 1024].into_iter().chain([0xff]).collect::<Vec<_>>()) },
        Output::Bytes { mime: "audio/wav".into(), bytes: Arc::from(b"RIFF\0\0\0\0WAVEfmt ".as_slice()) },
        Output::Bytes { mime: "image/png".into(), bytes: Arc::from(b"\0\x01\x02".as_slice()) },
    ] {
        let done = Completed::new(vec![image(png()), Output::Text("prefix".into()), late]);
        no_output_copy(&done, Limits::default());
    }
    let done = Completed::new(vec![image(png()), Output::Bytes { mime: "x".repeat(500), bytes: png() }]);
    no_output_copy(&done, Limits { outputs: 2000, ..Limits::default() });
    let mut route = routes();
    route.push(GenerateOutput { port: "p".repeat(500), modality: Modality::Text });
    let mut conversion = Conversion::default();
    assert!(
        outputs_with_limits(&route, &Completed::new(vec![Output::Text("x".into())]), Limits { outputs: 2000, ..Limits::default() }, &mut conversion).is_err()
    );
    assert_eq!(conversion, Conversion::default());
}

#[test]
fn borrowed_text_bytes_bom_routes_and_graph_text_parity() {
    let done = Completed::new(vec![Output::Bytes { mime: "  TeXt/plain".into(), bytes: Arc::from(b"\xef\xbb\xbf\xc3\xa9".as_slice()) }]);
    let got = outputs_with_limits(&routes(), &done, Limits { text: 2, raw_outputs: 5, ..Limits::default() }, &mut Conversion::default()).unwrap();
    assert_eq!(got.outputs, vec![("caption".into(), Value::Text("é".into()))]);
    no_output_copy(&done, Limits { text: 1, ..Limits::default() });
    no_output_copy(&done, Limits { raw_outputs: 4, ..Limits::default() });
    let mut route = routes();
    route.push(GenerateOutput { port: "second-text".into(), modality: Modality::Text });
    assert_eq!(produced(&route, &Completed::new(vec![Output::Text("x".into())])).unwrap().outputs[0].0, "caption");
    let generation = |size| nodecraft_graph::Generation {
        id: nodecraft_types::GenerationId::from_bytes([1; 16]),
        outputs: vec![Value::Text("x".repeat(size))],
        input_hash: None,
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
    };
    assert!(generation(MAX_TEXT_VALUE_BYTES).validate().is_ok());
    assert!(generation(MAX_TEXT_VALUE_BYTES + 1).validate().is_err());
}

#[test]
fn checked_arithmetic_rejects_overflow_without_large_allocations() {
    let mut budget = Budget(10);
    assert!(budget.charge(usize::MAX).is_err());
    assert_eq!(budget.0, 10);
    let mut budget = Budget(5);
    assert!(budget.string("é").is_err());
    assert_eq!(budget.0, 5);
}

#[test]
fn byte_text_over_raw_work_cap_refuses_before_full_utf8() {
    let done = Completed::new(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(&b"abcdefghijkl"[..]) }]);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    assert!(outputs_with_work(&routes(), &done, Limits { text: 8, ..Limits::default() }, &mut conversion, &mut work).is_err());
    assert_eq!(work.utf8_calls, 0);
    assert_eq!(work.utf8_bytes, 0);
    assert_eq!(conversion, Conversion::default());
}

#[test]
fn admitted_byte_text_validates_utf8_exactly_once() {
    let done = Completed::new(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(&b"hello"[..]) }]);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    let got = outputs_with_work(&routes(), &done, Limits::default(), &mut conversion, &mut work).unwrap();
    assert_eq!(work.utf8_calls, 1);
    assert_eq!(work.utf8_bytes, 5);
    assert!(conversion.strings > 0 && conversion.text_bytes > 0 && conversion.output_records > 0);
    assert_eq!(got.outputs, vec![("caption".into(), Value::Text("hello".into()))]);
}

fn probe_byte_text(bytes: Vec<u8>, mime: &str, limit: usize) -> (Result<Produced, Error>, ScanWork, Conversion) {
    let done = Completed::new(vec![Output::Bytes { mime: mime.into(), bytes: Arc::from(bytes) }]);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    let result = outputs_with_work(&routes(), &done, Limits { text: limit, ..Limits::default() }, &mut conversion, &mut work);
    (result, work, conversion)
}

#[test]
fn raw_text_work_bound_bom_and_utf8_priority_matrix() {
    for (len, ok, scans) in [(8, true, 1), (9, false, 1), (11, false, 1), (12, false, 0)] {
        let (result, work, conversion) = probe_byte_text(vec![b'x'; len], "text/plain", 8);
        assert_eq!(result.is_ok(), ok, "length {len}");
        assert_eq!(work.utf8_calls, scans, "length {len}");
        assert_eq!(work.utf8_bytes, len * scans);
        if !ok {
            assert_eq!(conversion, Conversion::default());
        }
    }
    let (result, work, _) = probe_byte_text([b"\xef\xbb\xbf".as_slice(), b"12345678"].concat(), "text/plain", 8);
    assert_eq!(result.unwrap().outputs[0].1, Value::Text("12345678".into()));
    assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 11));
    let boms = b"\xef\xbb\xbf".repeat(3);
    let (result, work, _) = probe_byte_text(boms.clone(), "text/plain", 6);
    assert_eq!(result.unwrap().outputs[0].1, Value::Text(String::new()));
    assert_eq!(work.utf8_calls, 1);
    let (result, work, conversion) = probe_byte_text([boms, b"x".to_vec()].concat(), "text/plain", 6);
    assert!(result.unwrap_err().message.contains("raw text"));
    assert_eq!(work.utf8_calls, 0);
    assert_eq!(conversion, Conversion::default());
    let (result, work, _) = probe_byte_text("éééé".as_bytes().to_vec(), "text/plain", 8);
    assert_eq!(result.unwrap().outputs[0].1, Value::Text("éééé".into()));
    assert_eq!(work.utf8_bytes, 8);
    for (mime, kind, message) in [("image/private-marker", ErrorKind::InvalidInput, "claimed non-text"), ("text/plain", ErrorKind::Unsupported, "UTF-16")] {
        let (result, work, conversion) = probe_byte_text([b"\xff\xfe".as_slice(), &[0; 20]].concat(), mime, 1);
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(error.message.contains(message));
        assert!(!error.message.contains("private-marker"));
        assert_eq!(work.utf8_calls, 0);
        assert_eq!(conversion, Conversion::default());
    }
    let (result, work, _) = probe_byte_text(b"valid".to_vec(), "text/plain", usize::MAX);
    assert!(result.unwrap_err().message.contains("overflow"));
    assert_eq!(work.utf8_calls, 0);
}

#[test]
fn late_invalid_utf8_keeps_caller_result_and_never_converts_prefix() {
    let bytes: Arc<[u8]> = Arc::from([vec![b'a'; 1024], vec![0xc3]].concat());
    let done = Completed::new(vec![image(png()), Output::Text("valid prefix".into()), Output::Bytes { mime: "text/plain".into(), bytes: Arc::clone(&bytes) }]);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    let error = outputs_with_work(&routes(), &done, Limits { text: 2048, ..Limits::default() }, &mut conversion, &mut work).unwrap_err();
    assert!(error.message.contains("isn't UTF-8"));
    assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 1025));
    assert_eq!(conversion, Conversion::default());
    assert_eq!(Arc::strong_count(&bytes), 2);
    assert_eq!(bytes.last(), Some(&0xc3));
    let valid = [vec![b'a'; 1023], vec![0xc3, 0xa9]].concat();
    let (result, work, conversion) = probe_byte_text(valid.clone(), "text/plain", 1025);
    assert_eq!(result.unwrap().outputs[0].1, Value::Text(String::from_utf8(valid).unwrap()));
    assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 1025));
    assert!(conversion.output_records > 0);
}

#[test]
fn missing_route_names_only_the_closed_modality() {
    let cases = [
        ("text", Output::Text("private-marker".into())),
        ("image", Output::Bytes { mime: "private-marker".into(), bytes: png() }),
        ("audio", Output::Bytes { mime: "private-marker".into(), bytes: Arc::from(b"RIFF\0\0\0\0WAVEfmt ".as_slice()) }),
        ("video", Output::Bytes { mime: "private-marker".into(), bytes: Arc::from(b"\0\0\0\x18ftypisom".as_slice()) }),
        ("mesh", Output::Bytes { mime: "private-marker".into(), bytes: Arc::from(b"glTF\x02\0\0\0".as_slice()) }),
        ("document", Output::Bytes { mime: "private-marker".into(), bytes: Arc::from(b"%PDF-1.7".as_slice()) }),
    ];
    for (label, item) in cases {
        let mut conversion = Conversion::default();
        let error = outputs_with_limits(&[], &Completed::new(vec![item]), Limits::default(), &mut conversion).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains(&format!("returned {label} output")), "{error}");
        assert!(!error.message.contains("private-marker"));
        assert_eq!(conversion, Conversion::default());
    }
}

#[test]
fn output_proof_storage_is_bounded_and_charged_before_semantic_scans() {
    let done = Completed::new(vec![Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(b"hello".as_slice()) }]);
    let mut work = ScanWork::default();
    let output_routes = routes();
    let proof = admit_outputs(&output_routes, &done, Limits::default(), &mut work).unwrap();
    assert_eq!(proof.items.len(), 1);
    // With no route cost: record128+MIME94+proof96=318, refused before scanning at317.
    let mut work = ScanWork::default();
    assert!(admit_outputs(&[], &done, Limits { outputs: 317, ..Limits::default() }, &mut work).is_err());
    assert_eq!(work.utf8_calls, 0);
    assert_eq!(OUTPUT_PROOF_RECORD_BYTES, 64);
    assert_eq!(OUTPUT_PROOF_ROOT_BYTES, 32);
}

#[test]
fn oversized_text_with_invalid_tail_is_rejected_by_raw_work_before_validation() {
    let bytes = [vec![b'a'; 1024], vec![0xff]].concat();
    let (result, work, conversion) = probe_byte_text(bytes, "text/plain", 8);
    assert!(result.unwrap_err().message.contains("raw text"));
    assert_eq!((work.utf8_calls, work.utf8_bytes), (0, 0));
    assert_eq!(conversion, Conversion::default());
}

fn aggregate_bom_refusal_control(shared: bool) {
    let first: Arc<[u8]> = Arc::from(b"\xef\xbb\xbf".repeat(3));
    let second = if shared { Arc::clone(&first) } else { Arc::from(b"\xef\xbb\xbf".repeat(3)) };
    let done = Completed::new(vec![
        Output::Bytes { mime: "text/plain".into(), bytes: Arc::clone(&first) },
        Output::Bytes { mime: "text/plain".into(), bytes: second },
    ]);
    let retained = Arc::strong_count(&first);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    let result = outputs_with_work(&routes(), &done, Limits { text: 6, ..Limits::default() }, &mut conversion, &mut work);
    assert!(result.is_err(), "two raw 9-byte BOM items exceed the 12-byte aggregate work budget: {result:?}");
    assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 9), "refuse before scanning the offending second item");
    assert_eq!(conversion, Conversion::default());
    assert_eq!(first.as_ref(), b"\xef\xbb\xbf".repeat(3));
    assert_eq!(Arc::strong_count(&first), retained);
}
#[test]
fn aggregate_raw_text_bom_distinct_buffers_refuse_before_second_scan() {
    aggregate_bom_refusal_control(false)
}
#[test]
fn aggregate_raw_text_bom_aliased_buffers_count_each_occurrence() {
    aggregate_bom_refusal_control(true)
}

#[test]
fn aggregate_raw_text_exact_boundary_and_one_over_do_not_convert_a_prefix() {
    for (extra, accepted) in [(false, true), (true, false)] {
        let first = b"\xef\xbb\xbf".repeat(2);
        let mut second = first.clone();
        if extra {
            second.push(b'x')
        }
        let done = Completed::new(vec![
            Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(first) },
            Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(second) },
        ]);
        let mut work = ScanWork::default();
        let mut conversion = Conversion::default();
        let result = outputs_with_work(&routes(), &done, Limits { text: 6, ..Limits::default() }, &mut conversion, &mut work);
        assert_eq!(result.is_ok(), accepted);
        assert!(work.utf8_bytes <= 6 + 3 * done.outputs.len());
        if accepted {
            assert_eq!((work.utf8_calls, work.utf8_bytes), (2, 12));
            assert_eq!(conversion.output_records, 2)
        } else {
            assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 6));
            assert_eq!(conversion, Conversion::default())
        }
    }
}
#[test]
fn aggregate_raw_text_bom_payloads_and_unicode_scan_once_within_the_joint_budget() {
    for (text, parts, expected) in [(6, vec!["\u{feff}abc", "\u{feff}def"], vec!["abc", "def"]), (4, vec!["\u{feff}é", "\u{feff}é"], vec!["é", "é"])] {
        let done = Completed::new(parts.iter().map(|s| Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(s.as_bytes()) }).collect());
        let mut work = ScanWork::default();
        let mut conversion = Conversion::default();
        let got = outputs_with_work(&routes(), &done, Limits { text, ..Limits::default() }, &mut conversion, &mut work).unwrap();
        assert_eq!(work.utf8_calls, 2);
        assert_eq!(work.utf8_bytes, text + 3 * done.outputs.len());
        assert_eq!(
            got.outputs.iter().map(|(_, v)| v).collect::<Vec<_>>(),
            expected.iter().map(|s| Value::Text((*s).into())).collect::<Vec<_>>().iter().collect::<Vec<_>>()
        );
        assert!(conversion.output_records > 0 && conversion.strings > 0);
    }
}
#[test]
fn aggregate_raw_text_invalid_tail_obeys_remaining_work_and_preserves_paid_result() {
    for (text, scan_tail) in [(1024, false), (1030, true)] {
        let tail: Arc<[u8]> = Arc::from([vec![b'a'; 1024], vec![0xff]].concat());
        let mut done = Completed::new(vec![
            Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(b"\xef\xbb\xbf".repeat(3)) },
            Output::Bytes { mime: "text/plain".into(), bytes: Arc::clone(&tail) },
        ]);
        done.usage.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
        let mut work = ScanWork::default();
        let mut conversion = Conversion::default();
        let error = outputs_with_work(&routes(), &done, Limits { text, ..Limits::default() }, &mut conversion, &mut work).unwrap_err();
        assert!(error.message.contains(if scan_tail { "isn't UTF-8" } else { "aggregate raw text" }));
        assert_eq!((work.utf8_calls, work.utf8_bytes), if scan_tail { (2, 1034) } else { (1, 9) });
        assert!(work.utf8_bytes <= text + 3 * done.outputs.len());
        assert_eq!(conversion, Conversion::default());
        assert_eq!(Arc::strong_count(&tail), 2);
        assert_eq!(tail.last(), Some(&0xff));
        assert_eq!(done.usage.cost.unwrap().micros, 7);
    }
}
#[test]
fn aggregate_raw_text_budget_does_not_rescan_direct_text_or_binary_assets() {
    let asset = png();
    let done = Completed::new(vec![
        Output::Text("ab".into()),
        image(Arc::clone(&asset)),
        Output::Bytes { mime: "text/plain".into(), bytes: Arc::from(b"\xef\xbb\xbf".as_slice()) },
    ]);
    let mut work = ScanWork::default();
    let mut conversion = Conversion::default();
    let got = outputs_with_work(&routes(), &done, Limits { text: 2, ..Limits::default() }, &mut conversion, &mut work).unwrap();
    assert_eq!((work.utf8_calls, work.utf8_bytes), (1, 3));
    assert!(Arc::ptr_eq(&got.assets[0].1, &asset));
    assert_eq!(conversion.output_records, 3);
}
#[test]
fn aggregate_raw_text_budget_arithmetic_is_checked_without_large_fixtures() {
    assert!(raw_text_budget(usize::MAX, 1).is_err());
    assert!(raw_text_budget(0, usize::MAX).is_err());
    let mut budget = raw_text_budget(6, 2).unwrap();
    assert_eq!(budget.0, 12);
    budget.charge(12).unwrap();
    assert!(budget.charge(1).is_err());
    assert_eq!(budget.0, 0);
}

#[test]
fn hdr394_provider_outputs_refuse_before_converting_any_paid_prefix() {
    for bytes in [b"\x76\x2f\x31\x01".as_slice(), b"#?RGBE\n", b"#?RADIANCE\n"] {
        for claimed in ["image/x-exr", "image/vnd.radiance", "text/plain"] {
            let hdr: Arc<[u8]> = Arc::from(bytes);
            let mut done =
                Completed::new(vec![image(png()), Output::Text("paid prefix".into()), Output::Bytes { mime: claimed.into(), bytes: Arc::clone(&hdr) }]);
            done.usage.cost = Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 7));
            let original = done.clone();
            let owners = Arc::strong_count(&hdr);
            let mut conversion = Conversion::default();
            let err = outputs_with_limits(&routes(), &done, Limits::default(), &mut conversion).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Unsupported);
            assert!(err.message.contains("HDR"));
            assert_eq!(conversion, Conversion::default());
            assert_eq!(done, original);
            assert_eq!(Arc::strong_count(&hdr), owners);
            assert_eq!(produced(&routes(), &done).unwrap_err().kind, ErrorKind::Unsupported);
        }
    }
}

#[test]
fn a_style_is_sent_only_when_its_bytes_match_the_family_its_card_records() {
    let style = nodecraft_media::style::Style {
        provider: "mock".into(),
        trainer: "mock-style".into(),
        family: "flux-1".into(),
        trigger: String::new(),
        weights: "https://mock.invalid/styles/a.safetensors".into(),
    };
    let bytes = nodecraft_media::style::encode(&style).unwrap();
    let asset = |mime: &str| AssetRef { hash: Blake3::from_bytes([2; 32]), mime: mime.parse().unwrap(), meta: Default::default() };
    assert!(style_matches(&asset(&style_mime("flux-1")), &bytes).is_ok());
    let relabelled = style_matches(&asset(&style_mime("sdxl-1")), &bytes).unwrap_err();
    assert!(relabelled.message.contains("doesn't match the family"), "{relabelled:?}");
    assert!(style_matches(&asset(nodecraft_types::STYLE_MIME), &bytes).is_err(), "a style with no family is never sent");
    assert!(style_matches(&asset(&style_mime("flux-1")), b"not a style").unwrap_err().message.contains("damaged"));
}
