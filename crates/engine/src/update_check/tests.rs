use super::*;

fn release(version: &str, prerelease: bool) -> Release {
    Release { version: version.into(), url: format!("https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v{version}"), prerelease }
}

fn listed(tag: &str, url: &str, prerelease: bool, draft: bool) -> Value {
    json!({"tag_name": tag, "html_url": url, "prerelease": prerelease, "draft": draft, "body": "notes", "assets": []})
}

#[test]
fn versions_order_as_semver_does() {
    let order =
        ["0.1.0-alpha", "0.1.0-alpha.1", "0.1.0-alpha.beta", "0.1.0-beta.2", "0.1.0-beta.11", "0.1.0-rc.1", "0.1.0", "0.1.1", "0.2.0-rc.1", "0.10.0", "1.0.0"];
    for pair in order.windows(2) {
        let (a, b) = (Version::parse(pair[0]).unwrap(), Version::parse(pair[1]).unwrap());
        assert!(a < b, "{} < {}", pair[0], pair[1]);
    }
    assert_eq!(Version::parse("1.2.3+build.7"), Version::parse("1.2.3"));
}

#[test]
fn hostile_versions_are_refused_not_panicked_on() {
    for bad in ["", "1", "1.2", "1.2.3.4", "1..3", "a.b.c", "1.2.3-", "1.2.3-rc..1", "1.2.3-rc_1", "99999999999999999999.0.0", "١.٢.٣", &"9".repeat(1000)] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_newest_release_on_the_channel_is_offered_only_when_newer() {
    let releases = [release("0.1.0", false), release("0.2.0-rc.1", true), release("0.2.0-rc.2", true), release("0.1.1", false)];
    assert_eq!(newest("0.1.0", &releases, UpdateChannel::Stable), Some(release("0.1.1", false)));
    assert_eq!(newest("0.1.0", &releases, UpdateChannel::Prerelease), Some(release("0.2.0-rc.2", true)));
    assert_eq!(newest("0.2.0-rc.2", &releases, UpdateChannel::Prerelease), None, "up to date");
    assert_eq!(newest("0.2.0-rc.1", &releases, UpdateChannel::Stable), None, "no finished 0.2.0 yet");
    assert_eq!(newest("not a version", &releases, UpdateChannel::Prerelease), None);
}

#[test]
fn drafts_odd_tags_and_foreign_pages_are_left_out() {
    let ours = "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v0.3.0";
    let body = serde_json::to_vec(&json!([
        listed("v0.3.0", ours, false, false),
        listed("v0.4.0", "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v0.4.0", false, true),
        listed("nightly", "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/nightly", false, false),
        listed("v9.0.0", "https://evil.example/releases/tag/v9.0.0", false, false),
        listed("v9.0.1", "https://github.com/someone-else/NodeCraft/releases/tag/v9.0.1", false, false),
        listed("v9.0.2", "https://github.com/CypherNaught-0x/NodeCraft/releases/", false, false),
        listed("v9.0.3", "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v9 0 3", false, false),
        listed("v0.3.1-rc.1", "https://github.com/cyphernaught-0x/nodecraft/releases/tag/v0.3.1-rc.1", false, false),
        {"tag_name": 5},
        "not an object",
    ]))
    .unwrap();
    let releases = releases(&body).unwrap();
    assert_eq!(
        releases,
        vec![
            Release { version: "0.3.0".into(), url: ours.into(), prerelease: false },
            // A pre-release version is one, whatever the flag says.
            Release { version: "0.3.1-rc.1".into(), url: "https://github.com/cyphernaught-0x/nodecraft/releases/tag/v0.3.1-rc.1".into(), prerelease: true },
        ]
    );
}

#[test]
fn a_reply_that_isnt_a_release_list_is_an_error() {
    for body in [&b"{}"[..], b"<html>", b"", b"[1, 2"] {
        assert_eq!(releases(body).map_err(|e| e.kind), Err(ErrorKind::InvalidInput), "{:?}", String::from_utf8_lossy(body));
    }
    let many: Vec<Value> =
        (0..1000).map(|n| listed(&format!("v0.0.{n}"), &format!("https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v0.0.{n}"), false, false)).collect();
    assert_eq!(releases(&serde_json::to_vec(&many).unwrap()).unwrap().len(), MAX_RELEASES);
}

fn new_session() -> Session {
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([3; 16]), "P").unwrap()).unwrap()
}

struct Reply(u16, Vec<u8>, std::sync::Mutex<Vec<HttpRequest>>);

impl Http for Reply {
    fn send(&self, request: HttpRequest) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        if let Ok(mut sent) = self.2.lock() {
            sent.push(request);
        }
        let response = nodecraft_providers::HttpResponse { status: self.0, headers: Vec::new(), body: self.1.clone() };
        Box::pin(async move { Ok(response) })
    }
}

#[test]
fn fetching_asks_github_for_nothing_but_the_release_list() {
    let body = serde_json::to_vec(&json!([listed("v99.0.0", "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v99.0.0", false, false)])).unwrap();
    let http = Reply(200, body, Default::default());
    let fetched = futures::executor::block_on(fetch(&http)).unwrap();
    assert_eq!(fetched, vec![release("99.0.0", false)]);
    let sent = http.2.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url, RELEASES_URL);
    assert!(sent[0].body.is_empty() && sent[0].secret_headers.is_empty());
    assert_eq!(sent[0].max_response_bytes, MAX_REPLY_BYTES);

    let refused = futures::executor::block_on(fetch(&Reply(403, b"rate limited".to_vec(), Default::default()))).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Network);
    assert_eq!(refused.message, "GitHub answered 403");
}

#[test]
fn a_host_without_the_service_refuses_and_status_reports_the_last_check() {
    let mut session = new_session();
    let refused = session.dispatch("app.check_updates", &json!({})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert_eq!(session.dispatch("app.update_status", &json!({})).unwrap(), json!({"current": VERSION, "last": null}));
    assert!(session.dispatch("app.update_status", &json!({"extra": 1})).is_err());

    let check = finish(&mut session, 7, UpdateChannel::Stable, Ok(vec![release("99.0.0", false)]));
    let status = session.dispatch("app.update_status", &json!({})).unwrap();
    assert_eq!(status["last"], serde_json::to_value(&check).unwrap());
    assert_eq!(status["last"]["available"]["version"], "99.0.0");

    let mut session = new_session();
    session.register_host_service(UPDATE_SERVICE, |_| Ok(json!({"pending": 1}))).unwrap();
    assert_eq!(session.dispatch("app.check_updates", &json!({})).unwrap(), json!({"pending": 1}));
}
