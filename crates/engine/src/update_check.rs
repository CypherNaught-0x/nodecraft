//! Checking for a newer release (#123): notify only, never install.
//!
//! `app.check_updates` asks the host's [`UPDATE_SERVICE`] to read NodeCraft's releases from
//! GitHub; the host runs [`fetch`] off the UI thread and hands the outcome to [`finish`], which
//! picks the newest release on the person's [`UpdateChannel`] that is newer than this build and
//! records it for `app.update_status`. The request carries nothing but the generic
//! `NodeCraft/<version>` User-Agent. A reply is hostile input: it is size-capped, a tag that
//! isn't a clean version is ignored, and the link the app offers to open is built from that
//! version rather than taken from the reply, so it always leads to NodeCraft's releases page.
//! Handlers do no I/O.
use crate::build_info::VERSION;
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use crate::session::Session;
use nodecraft_providers::http::{Http, HttpMethod, HttpRequest};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Ordering;

/// The host service that reads the release list.
pub const UPDATE_SERVICE: &str = "updates.fetch";

/// NodeCraft's releases, newest first.
pub const RELEASES_URL: &str = "https://api.github.com/repos/CypherNaught-0x/NodeCraft/releases?per_page=100";

/// A release's page is this followed by its tag.
const RELEASE_TAGS: &str = "https://github.com/CypherNaught-0x/NodeCraft/releases/tag/";

const MAX_REPLY_BYTES: usize = 4 * 1024 * 1024;
const MAX_RELEASES: usize = 100;
const MAX_FIELD_BYTES: usize = 512;
const TIMEOUT_MS: u64 = 15_000;

/// Which releases a person is told about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Finished releases only.
    #[default]
    Stable,
    /// Release candidates (`-rc.N`) too.
    Prerelease,
}

/// One published release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Release {
    /// Its version, without the tag's `v` or build metadata.
    pub version: String,
    /// Its page on GitHub, with the notes and downloads.
    pub url: String,
    pub prerelease: bool,
}

impl Release {
    /// Its tag, read back from the page built from it.
    pub fn tag(&self) -> Option<&str> {
        self.url.strip_prefix(RELEASE_TAGS).filter(|tag| !tag.is_empty())
    }
}

/// A release newer than this build.
pub type Available = Release;

/// The outcome of one check, which `app.update_status` reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: u64,
    /// This build's version.
    pub current: String,
    pub channel: UpdateChannel,
    /// The newest release on the channel, when it is newer than this build.
    pub available: Option<Available>,
    /// Why the releases couldn't be read.
    pub error: Option<String>,
}

/// The last check, a session resource.
struct LastCheck(Check);

/// Reads NodeCraft's releases. The host calls it off the UI thread.
pub async fn fetch(http: &dyn Http) -> Result<Vec<Release>, Error> {
    fetch_from(http, RELEASES_URL).await
}

/// Reads a release list from `url`: [`RELEASES_URL`], or a test feed's list.
pub async fn fetch_from(http: &dyn Http, url: &str) -> Result<Vec<Release>, Error> {
    let request = HttpRequest {
        method: HttpMethod::Get,
        url: url.into(),
        headers: vec![("accept".into(), "application/vnd.github+json".into())],
        secret_headers: Vec::new(),
        body: Vec::new(),
        timeout_ms: TIMEOUT_MS,
        max_response_bytes: MAX_REPLY_BYTES,
    };
    let response = http.send(request).await.map_err(|error| Error::new(ErrorKind::Network, format!("GitHub couldn't be reached: {}", error.message)))?;
    if response.status != 200 {
        return Err(Error::new(ErrorKind::Network, format!("GitHub answered {}", response.status)));
    }
    releases(&response.body)
}

/// The releases in a GitHub release list. Drafts and releases whose tag isn't a version are left
/// out; each page is built from its tag, never read from the reply.
pub fn releases(body: &[u8]) -> Result<Vec<Release>, Error> {
    #[derive(Deserialize)]
    struct Listed {
        tag_name: String,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        draft: bool,
    }
    let listed: Vec<Value> = serde_json::from_slice(body).map_err(|_| Error::new(ErrorKind::InvalidInput, "GitHub's release list couldn't be read"))?;
    let mut releases = Vec::new();
    for value in listed.into_iter().take(MAX_RELEASES) {
        let Ok(release) = serde_json::from_value::<Listed>(value) else { continue };
        if release.draft {
            continue;
        }
        let tagged = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
        // Parsing admits only digits, ASCII letters, `.`, `-` and `+`, so the tag is safe in a url.
        let Some(parsed) = Version::parse(tagged) else { continue };
        let version = tagged.split_once('+').map_or(tagged, |(version, _)| version);
        let url = format!("{RELEASE_TAGS}{}", release.tag_name);
        releases.push(Release { version: version.to_owned(), url, prerelease: release.prerelease || !parsed.pre.is_empty() });
    }
    Ok(releases)
}

/// The newest release on `channel` that is newer than `current`.
pub fn newest(current: &str, releases: &[Release], channel: UpdateChannel) -> Option<Available> {
    let current = Version::parse(current)?;
    releases
        .iter()
        .filter(|release| channel == UpdateChannel::Prerelease || !release.prerelease)
        .filter_map(|release| Some((Version::parse(&release.version)?, release)))
        .filter(|(version, _)| *version > current)
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, release)| release.clone())
}

/// Whether `version` is newer than `current`; false when either isn't a version.
pub fn is_newer(version: &str, current: &str) -> bool {
    matches!((Version::parse(version), Version::parse(current)), (Some(version), Some(current)) if version > current)
}

/// Records what check `id` brought back for `app.update_status`.
pub fn finish(session: &mut Session, id: u64, channel: UpdateChannel, fetched: Result<Vec<Release>, Error>) -> Check {
    let (available, error) = match fetched {
        Ok(releases) => (newest(VERSION, &releases, channel), None),
        Err(error) => (None, Some(error.message)),
    };
    let check = Check { id, current: VERSION.into(), channel, available, error };
    session.swap_resource(Some(LastCheck(check.clone())));
    check
}

/// A semantic version (`1.2.3`, `0.2.0-rc.1`); build metadata is checked, then ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    pre: Vec<Identifier>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    // Numeric identifiers sort before alphanumeric ones.
    Number(u64),
    Text(String),
}

impl Version {
    fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > MAX_FIELD_BYTES {
            return None;
        }
        let (text, build) = text.split_once('+').map_or((text, None), |(version, build)| (version, Some(build)));
        if build.is_some_and(|build| !build.split('.').all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))) {
            return None;
        }
        let (core, pre) = text.split_once('-').map_or((text, None), |(core, pre)| (core, Some(pre)));
        let mut parts = core.split('.');
        let mut number = || parts.next().filter(|p| is_number(p)).and_then(|p| p.parse::<u64>().ok());
        let core = [number()?, number()?, number()?];
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        None
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        is_number(id).then(|| id.parse().ok().map(Identifier::Number)).flatten()
                    } else {
                        Some(Identifier::Text(id.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self { core, pre })
    }
}

/// Digits without a leading zero, as semver numbers are written.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && (text == "0" || !text.starts_with('0'))
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            // A release sorts after its pre-releases.
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

fn enabled_check(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    if !c.services.contains(UPDATE_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host doesn't check for updates; the desktop app does"));
    }
    Ok(())
}

fn check(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_check(c, value)?;
    Ok(Effect::Service { capability: UPDATE_SERVICE, request: json!({}) })
}

fn enabled_status(_: &Context<'_>, value: &Value) -> Result<(), Error> {
    let _: NoParams = params(value)?;
    Ok(())
}

fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: NoParams = params(value)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    let last = c.resources.get::<LastCheck>().map(|last| &last.0);
    Ok(Effect::None { result: json!({"current": VERSION, "last": last, "install": crate::update_install::last(c)}) })
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "properties": {}, "required": []})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "app.check_updates",
            label: "Check for Updates…",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {pending: id}; reads NodeCraft's releases from GitHub and reports the newest one on the update channel in settings that is newer than this build. Nothing is downloaded or installed; app.install_update does that. The host answers later: read the outcome with app.update_status. Refused when the host can't check (the CLI, the browser build).",
            enabled: enabled_check,
            run: check,
        },
        CommandSpec {
            id: "app.update_status",
            label: "Update status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {current, last: {id, current, channel: \"stable\"|\"prerelease\", available: {version, url, prerelease} | null, error} | null, install: {state: \"downloading\"|\"ready\"|\"failed\", version, error?} | null}; this build's version, the outcome of the last app.check_updates (null before one finishes) and where app.install_update stands. Read-only.",
            enabled: enabled_status,
            run: status,
        },
    ]
}

#[cfg(test)]
mod tests;
