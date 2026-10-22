//! The owner's share links (#121): each lets whoever holds it into the room in one role, until
//! it expires or is taken back. The key is shown once, when it is made; afterwards a link is
//! known by its id, the hash the relay keeps, so a list of links never holds a working one.
use serde::Deserialize;
use serde_json::{Value, json};

use nodecraft_sync::{Frame, Grant, LinkKey, MAX_LINKS, Role};
use nodecraft_types::{Error, ErrorKind};

use super::Collab;

/// The host service the `share.link_*` commands ask: a host sharing a session registers it
/// and passes each request to [`Collab::links_request`].
pub const LINKS_SERVICE: &str = "share.links";

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List,
    Create {
        role: Role,
        name: String,
        #[serde(default)]
        expires: Option<u64>,
    },
    Update {
        id: String,
        role: Role,
        name: String,
        #[serde(default)]
        expires: Option<u64>,
    },
    Revoke {
        id: String,
    },
}

impl Collab {
    /// Carries out one `share.links` request at `now` (seconds since 1970); the frames it
    /// needs go out at the next publish.
    pub fn links_request(&mut self, request: &Value, now: u64) -> Result<Value, Error> {
        if self.role != Some(Role::Owner) {
            return Err(Error::new(ErrorKind::Forbidden, "only who shared the project manages its share links"));
        }
        let request: Request =
            serde_json::from_value(request.clone()).map_err(|_| Error::new(ErrorKind::InvalidInput, "a share link request isn't readable"))?;
        match request {
            Request::List => {
                let links: Vec<Value> = self.links.iter().map(|(hash, grant)| entry(hash, grant, now)).collect();
                Ok(json!({"links": links, "max": MAX_LINKS}))
            }
            Request::Create { role, name, expires } => {
                let grant = terms(role, name, expires, now)?;
                if self.links.len() >= MAX_LINKS {
                    return Err(Error::new(ErrorKind::Overflow, "the project has as many share links as it may"));
                }
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "share link entropy is unavailable"))?;
                let key = LinkKey::from_bytes(bytes);
                let hash = key.hash();
                self.put(hash, grant.clone());
                let mut out = entry(&hash, &grant, now);
                if let Value::Object(fields) = &mut out {
                    fields.insert("key".into(), Value::String(key.as_str().to_owned()));
                }
                Ok(out)
            }
            Request::Update { id, role, name, expires } => {
                let hash = known(&self.links, &id)?;
                let grant = terms(role, name, expires, now)?;
                self.put(hash, grant.clone());
                Ok(entry(&hash, &grant, now))
            }
            Request::Revoke { id } => {
                let hash = known(&self.links, &id)?;
                self.links.remove(&hash);
                self.revoked.insert(hash);
                self.outbox.push(Frame::LinkRevoke(hash));
                Ok(json!({"id": id, "revoked": true}))
            }
        }
    }

    fn put(&mut self, hash: [u8; 32], grant: Grant) {
        self.revoked.remove(&hash);
        self.links.insert(hash, grant.clone());
        self.outbox.push(Frame::LinkPut { hash, grant });
    }
}

fn terms(role: Role, name: String, expires: Option<u64>, now: u64) -> Result<Grant, Error> {
    let grant = Grant { role, name, expires };
    grant.validate()?;
    if !grant.live(now) {
        return Err(Error::new(ErrorKind::InvalidInput, "a share link's expiry is in the future"));
    }
    Ok(grant)
}

fn known(links: &std::collections::BTreeMap<[u8; 32], Grant>, id: &str) -> Result<[u8; 32], Error> {
    links.keys().find(|hash| hex(hash) == id).copied().ok_or_else(|| Error::new(ErrorKind::NotFound, "no share link has that id"))
}

fn entry(hash: &[u8; 32], grant: &Grant, now: u64) -> Value {
    json!({"id": hex(hash), "role": grant.role.as_str(), "name": grant.name, "expires": grant.expires, "expired": !grant.live(now)})
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
