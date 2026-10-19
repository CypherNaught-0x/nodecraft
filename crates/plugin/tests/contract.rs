//! The rules beyond decoding: dependency locks, negotiation, staged registration, job order and
//! what projects keep about plugins. Every decoder is also fuzzed here.
use nodecraft_plugin::frame::{FrameDecoder, GuestErrorCode, Message, Stream};
use nodecraft_plugin::id::{LocalId, PackageId, Token};
use nodecraft_plugin::job::{JobTracker, Verdict};
use nodecraft_plugin::lock::{Installed, resolve};
use nodecraft_plugin::manifest::{ContractVersion, Dependency, Features, ProtocolRange, Version};
use nodecraft_plugin::negotiate::{HostVersions, negotiate};
use nodecraft_plugin::preserve::{Budget, DescriptorSnapshot, OpaqueConfig, Requirement};
use nodecraft_plugin::{Code, Inventory, Manifest, PackageDigest, json, register};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn manifest(id: &str) -> Manifest {
    let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/manifest/valid/data.json")).unwrap();
    let mut m = Manifest::decode(&bytes).unwrap();
    m.package_id = PackageId::parse(id).unwrap();
    m.dependencies.clear();
    m
}

fn lock(m: &mut Manifest, on: &Manifest, digest: u8) {
    m.dependencies.push(Dependency { package_id: on.package_id.clone(), version: on.version, digest: PackageDigest([digest; 32]) });
}

/// Package `i` of `chain`, installed with digest `[i; 32]`.
fn installed(chain: &[Manifest]) -> BTreeMap<PackageId, Installed<'_>> {
    chain.iter().enumerate().map(|(i, m)| (m.package_id.clone(), Installed { manifest: m, digest: PackageDigest([i as u8; 32]) })).collect()
}

#[test]
fn locks_are_exact_acyclic_shallow_and_data_only() {
    let mut chain: Vec<Manifest> = (0..10).map(|i| manifest(&format!("io.example.p{i}"))).collect();
    for i in 0..9 {
        let next = chain[i + 1].clone();
        lock(&mut chain[i], &next, i as u8 + 1);
    }
    // p1 → … → p9 is 8 deep below p1: allowed, dependencies first.
    let order = resolve(&chain[1], &installed(&chain)).unwrap();
    assert_eq!(order.first().unwrap().as_str(), "io.example.p9");
    assert_eq!(order.len(), 8);
    // p0 adds a ninth level.
    assert_eq!(resolve(&chain[0], &installed(&chain)).unwrap_err().code, Code::DependencyRejected);
    // The wrong digest isn't the locked package.
    let mut wrong = chain.clone();
    wrong[8].dependencies[0].digest = PackageDigest([0xee; 32]);
    assert_eq!(resolve(&wrong[8], &installed(&wrong)).unwrap_err().code, Code::DependencyMissing);
    // Missing outright.
    assert_eq!(resolve(&chain[8], &BTreeMap::new()).unwrap_err().code, Code::DependencyMissing);
    // A cycle.
    let mut cycle = vec![manifest("io.example.a"), manifest("io.example.b")];
    let b = cycle[1].clone();
    lock(&mut cycle[0], &b, 1);
    let a = cycle[0].clone();
    lock(&mut cycle[1], &a, 0);
    assert_eq!(resolve(&cycle[0], &installed(&cycle)).unwrap_err().code, Code::DependencyCycle);
    // Executable packages can't be depended on.
    let mut exec = vec![manifest("io.example.app"), manifest("io.example.lib")];
    exec[1].runtime = nodecraft_plugin::manifest::Runtime::RustWasm { module: nodecraft_plugin::inventory::InventoryPath::parse("g.wasm").unwrap() };
    let lib = exec[1].clone();
    lock(&mut exec[0], &lib, 1);
    assert_eq!(resolve(&exec[0], &installed(&exec)).unwrap_err().code, Code::DependencyRejected);
}

#[test]
fn negotiation_takes_the_highest_common_minor_and_known_features() {
    let mut host = HostVersions::current();
    host.protocol.max_minor = 2;
    host.features.insert("streaming.chunks".into());
    let range = |a, b| ProtocolRange { major: 1, min_minor: a, max_minor: b };
    let c10 = ContractVersion { major: 1, minor: 0 };
    let mut features = Features::default();
    features.optional.insert("streaming.chunks".into());
    features.optional.insert("from.the.future".into());
    let agreed = negotiate(&host, range(1, 5), c10, &features).unwrap();
    assert_eq!(agreed.protocol_minor, 2);
    assert_eq!(agreed.features, BTreeSet::from(["streaming.chunks".to_owned()]), "unknown optional features drop out");
    assert_eq!(negotiate(&host, range(3, 5), c10, &features).unwrap_err().code, Code::Incompatible);
    assert_eq!(negotiate(&host, ProtocolRange { major: 2, ..range(0, 0) }, c10, &features).unwrap_err().code, Code::Incompatible);
    assert_eq!(negotiate(&host, range(0, 0), ContractVersion { major: 1, minor: 1 }, &features).unwrap_err().code, Code::Incompatible);
    features.required.insert("from.the.future".into());
    assert_eq!(negotiate(&host, range(0, 0), c10, &features).unwrap_err().code, Code::UnsupportedFeature);
}

#[test]
fn registration_is_all_or_nothing() {
    let m = manifest("io.example.palette");
    let staged = register::stage(&m, &BTreeSet::new()).unwrap();
    assert_eq!(staged.len(), 2);
    let mut taken: BTreeSet<String> = BTreeSet::from(["node.add".into()]);
    taken.insert(staged[1].to_string());
    assert_eq!(register::stage(&m, &taken).unwrap_err().code, Code::Collision);
    // Two packages that spell the same ID: whichever comes second is refused whole.
    let mut short = manifest("io.example.text");
    short.registrations.truncate(1);
    short.registrations[0].id = LocalId::parse("pack.x").unwrap();
    let mut long = manifest("io.example.text.pack");
    long.registrations.truncate(1);
    long.registrations[0].id = LocalId::parse("x").unwrap();
    let first: BTreeSet<String> = register::stage(&short, &BTreeSet::new()).unwrap().iter().map(|i| i.to_string()).collect();
    assert_eq!(register::stage(&long, &first).unwrap_err().code, Code::Collision);
    assert!(!register::host_id_allowed("plugin.io.example.x.y"));
    assert!(register::host_id_allowed("node.add"));
}

fn t(s: &str) -> Token {
    Token::parse(s).unwrap()
}

#[test]
fn jobs_follow_their_order_and_cancellation_wins() {
    let job = t("job-1");
    let progress = |v| Message::Progress { job_id: t("job-1"), value: v, note: None };
    let result = |epoch| Message::Result { job_id: t("job-1"), grant_epoch: epoch, outputs: vec![], attach_to: t("node") };
    let mut jobs = JobTracker::default();
    assert_eq!(jobs.observe(&Message::Accept { job_id: job.clone() }).unwrap_err().code, Code::Sequence, "not invoked");
    jobs.invoked(job.clone(), 3).unwrap();
    assert_eq!(jobs.invoked(job.clone(), 3).unwrap_err().code, Code::Sequence, "IDs aren't reused while open");
    assert_eq!(jobs.observe(&progress(10)).unwrap_err().code, Code::Sequence, "progress before accept");
    assert_eq!(jobs.observe(&Message::Accept { job_id: job.clone() }).unwrap(), Verdict::Apply);
    assert_eq!(jobs.observe(&Message::Accept { job_id: job.clone() }).unwrap_err().code, Code::Sequence);
    assert_eq!(jobs.observe(&progress(10)).unwrap(), Verdict::Apply);
    assert_eq!(jobs.observe(&progress(10)).unwrap(), Verdict::Apply);
    assert_eq!(jobs.observe(&progress(9)).unwrap_err().code, Code::Sequence, "never backwards");
    assert_eq!(jobs.observe(&result(3)).unwrap(), Verdict::Apply);
    assert_eq!(jobs.observe(&result(3)).unwrap_err().code, Code::Sequence, "exactly one terminal");
    assert_eq!(jobs.open(), 0);
    // A result for another grant epoch is refused (and closes the job).
    jobs.invoked(job.clone(), 4).unwrap();
    jobs.observe(&Message::Accept { job_id: job.clone() }).unwrap();
    assert_eq!(jobs.observe(&result(3)).unwrap_err().code, Code::Sequence);
    assert_eq!(jobs.open(), 0);
    // After cancel, everything is discarded; the ack closes it.
    jobs.invoked(job.clone(), 5).unwrap();
    jobs.observe(&Message::Accept { job_id: job.clone() }).unwrap();
    jobs.cancelled(&job).unwrap();
    assert_eq!(jobs.observe(&progress(900)).unwrap(), Verdict::Discard);
    assert_eq!(jobs.observe(&result(5)).unwrap(), Verdict::Discard, "late output is dropped");
    assert_eq!(jobs.observe(&Message::CancelAck { job_id: job.clone() }).unwrap(), Verdict::Discard);
    assert_eq!(jobs.open(), 0);
    // A guest may refuse before accepting.
    jobs.invoked(job.clone(), 6).unwrap();
    let refuse = Message::Error { job_id: job.clone(), code: GuestErrorCode::InvalidInput, field_ids: vec![], message: "no".into() };
    assert_eq!(jobs.observe(&refuse).unwrap(), Verdict::Apply);
    let _ = Stream { role: t("text"), handle: t("h"), bytes: 0 };
}

#[test]
fn projects_keep_plugin_data_byte_for_byte() {
    let payload = b"{ \"zeta\" : 1.50, \"alpha\":[1e3, \"\\u00e9\"] }";
    let config = OpaqueConfig::new(2, payload).unwrap();
    // Through an envelope that is parsed and re-emitted canonically, as a project save does.
    let envelope = json::canonical(&config.to_envelope()).unwrap();
    let reread = json::parse(envelope.as_bytes(), json::Limits::WIRE).unwrap();
    let back = OpaqueConfig::from_envelope(reread).unwrap();
    assert_eq!(back.payload(), payload);
    assert_eq!(back.version, 2);
    for (bad, code) in [(&b"{\"a\":1,\"a\":2}"[..], Code::DuplicateKey), (b"[", Code::Syntax), (b"\xff", Code::Syntax)] {
        assert_eq!(OpaqueConfig::new(1, bad).unwrap_err().code, code);
    }
    assert_eq!(OpaqueConfig::new(1, &vec![b' '; 64 * 1024 + 1]).unwrap_err().code, Code::TooLarge);
    // Snapshots share storage by digest; the project budget is bounded.
    let snap = DescriptorSnapshot::of(b"{ \"b\":1, \"a\":2 }").unwrap();
    assert_eq!(snap.canonical, r#"{"a":2,"b":1}"#);
    assert_eq!(snap, DescriptorSnapshot::of(br#"{"a":2,"b":1}"#).unwrap());
    let mut budget = Budget::default();
    for _ in 0..1000 {
        budget.snapshot(&snap).unwrap();
    }
    let big = OpaqueConfig::new(1, format!("\"{}\"", "x".repeat(60 * 1024)).as_bytes()).unwrap();
    let mut fits = 0;
    while budget.config(&big).is_ok() {
        fits += 1;
    }
    assert_eq!(fits, 8 * 1024 * 1024 / (60 * 1024 + 2));
    // A requirement round-trips and refuses IDs from another package.
    let m = manifest("io.example.palette");
    let used: BTreeSet<_> = m.registered_ids().collect();
    let req = Requirement::for_package(&m, PackageDigest([1; 32]), used).unwrap();
    let text = json::canonical(&req.to_json()).unwrap();
    assert_eq!(Requirement::from_json(json::parse(text.as_bytes(), json::Limits::WIRE).unwrap()).unwrap(), req);
    let other = manifest("io.example.other");
    assert_eq!(Requirement::for_package(&other, PackageDigest([1; 32]), m.registered_ids().collect()).unwrap_err().code, Code::BadId);
    assert!(!text.contains('/'), "no paths in a requirement");
}

#[test]
fn versions_are_strict() {
    assert_eq!(Version::parse("1.2.3").unwrap(), Version { major: 1, minor: 2, patch: 3 });
    for bad in ["1.2", "1.2.3.4", "v1.2.3", "1.2.3-rc.1", "1.2.3+b", "01.2.3", "1..3", "4294967296.0.0", ""] {
        assert_eq!(Version::parse(bad).unwrap_err().code, Code::BadVersion, "{bad}");
    }
}

proptest! {
    #[test]
    fn decoders_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let _ = Manifest::decode(&bytes);
        let _ = Message::decode(&bytes);
        let _ = Inventory::decode(&bytes);
        let _ = nodecraft_plugin::signature::PackageSignature::decode(&bytes);
        let _ = nodecraft_plugin::signature::TrustStore::decode(&bytes);
        let _ = OpaqueConfig::new(1, &bytes);
        let _ = DescriptorSnapshot::of(&bytes);
        let mut decoder = FrameDecoder::default();
        for chunk in bytes.chunks(7) {
            if decoder.push(chunk).is_err() { break; }
            while let Ok(Some(payload)) = decoder.next_frame() {
                let _ = Message::decode(&payload);
            }
        }
    }

    #[test]
    fn mutated_fixtures_never_panic(i in 0usize..4096, byte in any::<u8>()) {
        for name in ["manifest/valid/action.json", "manifest/valid/data.json", "frame/valid/invoke.json", "frame/valid/result.json", "package/textpack/inventory.json", "package/textpack/signature.json", "package/trust.json"] {
            let mut bytes = std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let at = i % bytes.len();
            bytes[at] = byte;
            let _ = Manifest::decode(&bytes);
            let _ = Message::decode(&bytes);
            let _ = Inventory::decode(&bytes);
            let _ = nodecraft_plugin::signature::PackageSignature::decode(&bytes);
            let _ = nodecraft_plugin::signature::TrustStore::decode(&bytes);
        }
    }
}
