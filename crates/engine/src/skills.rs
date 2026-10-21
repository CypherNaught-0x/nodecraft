//! The person's own agent skills (#168), kept outside any project so every project can use them
//! (`docs/decisions/2026-10-21-agent-skills.md`). A project's skills live in its document
//! ([`nodecraft_graph::Project::skills`]); these live in `skills.json` in the host's data folder
//! ([`FileSkills`]), or in memory in the browser ([`MemorySkills`]).
//!
//! Reading goes straight to the store ([`UserSkills`]), so the agent can look skills up without
//! asking anyone. Changing them goes through the host's `skills.store` service ([`handle`]),
//! which re-reads under the store's lock and refuses an edit made against a version that has
//! since changed, so two windows (or the app and the CLI) never lose each other's work.
use crate::session::Session;
use nodecraft_graph::{Requires, Skill, SkillSource, skill_name_key, validate_skills};
use nodecraft_types::{Error, ErrorKind, SkillId};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// The host service the `skill.*` commands ask to change the person's skills.
pub const SKILLS_SERVICE: &str = "skills.store";
/// The skills file's format version.
pub const SKILLS_FILE_VERSION: u32 = 1;
/// Largest skills file read back or written.
pub const MAX_SKILLS_FILE_BYTES: usize = 16 * 1024 * 1024;
/// What an exported skill says it is.
pub const SKILL_FORMAT: &str = "nodecraft.skill";
/// The exported skill format's version.
pub const SKILL_FORMAT_VERSION: u32 = 1;
/// Largest exported skill imported.
pub const MAX_SKILL_EXPORT_BYTES: usize = 256 * 1024;

/// The skills, by id.
pub type Skills = BTreeMap<SkillId, Skill>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillsFile {
    version: u32,
    #[serde(default)]
    skills: Vec<Skill>,
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// A skills file, checked as a project's skills are.
pub fn from_bytes(bytes: &[u8]) -> Result<Skills, Error> {
    if bytes.len() > MAX_SKILLS_FILE_BYTES {
        return Err(invalid("the skills file is too large"));
    }
    // The version first, so a newer file says so instead of looking damaged.
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let Version { version } = serde_json::from_slice(bytes).map_err(|_| invalid("the skills file is damaged"))?;
    if version != SKILLS_FILE_VERSION {
        return Err(Error::new(ErrorKind::Unsupported, "the skills file was written by a newer NodeCraft"));
    }
    let file: SkillsFile = serde_json::from_slice(bytes).map_err(|_| invalid("the skills file is damaged"))?;
    let mut skills = Skills::new();
    for skill in file.skills {
        if skills.insert(skill.id, skill).is_some() {
            return Err(invalid("the skills file holds one skill twice"));
        }
    }
    validate_skills(&skills)?;
    Ok(skills)
}

pub fn to_bytes(skills: &Skills) -> Result<Vec<u8>, Error> {
    let file = SkillsFile { version: SKILLS_FILE_VERSION, skills: skills.values().cloned().collect() };
    let bytes = serde_json::to_vec(&file).map_err(|_| Error::new(ErrorKind::Internal, "the skills file couldn't be written"))?;
    if bytes.len() > MAX_SKILLS_FILE_BYTES {
        return Err(invalid("your skills would be too large to keep; remove ones you no longer use"));
    }
    Ok(bytes)
}

/// A skill as it travels between people: what it says and needs, not who keeps it or how often
/// it was edited. Nothing else is accepted, so a file can't smuggle in anything that runs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Exported {
    format: String,
    version: u32,
    skill: Portable,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Portable {
    name: String,
    #[serde(default)]
    description: String,
    instructions: String,
    #[serde(default)]
    requires: Requires,
}

/// `skill` as an exported file's text.
pub fn export(skill: &Skill) -> Result<String, Error> {
    let portable = Portable {
        name: skill.name.clone(),
        description: skill.description.clone(),
        instructions: skill.instructions.clone(),
        requires: skill.requires.clone(),
    };
    serde_json::to_string_pretty(&Exported { format: SKILL_FORMAT.into(), version: SKILL_FORMAT_VERSION, skill: portable })
        .map_err(|_| Error::new(ErrorKind::Internal, "the skill couldn't be exported"))
}

/// An exported skill's `text`, as a new skill `id` from `origin`: imported, so untrusted, and
/// off until the person turns it on.
pub fn import(text: &str, id: SkillId, origin: &str) -> Result<Skill, Error> {
    if text.len() > MAX_SKILL_EXPORT_BYTES {
        return Err(invalid("that skill file is too large"));
    }
    #[derive(Deserialize)]
    struct Header {
        format: String,
        version: u32,
    }
    let header: Header = serde_json::from_str(text).map_err(|_| invalid("that isn't a NodeCraft skill file"))?;
    if header.format != SKILL_FORMAT {
        return Err(invalid("that isn't a NodeCraft skill file"));
    }
    if header.version != SKILL_FORMAT_VERSION {
        return Err(Error::new(ErrorKind::Unsupported, "that skill was exported by a newer NodeCraft"));
    }
    let exported: Exported = serde_json::from_str(text).map_err(|_| invalid("that skill file has fields NodeCraft doesn't accept"))?;
    let Portable { name, description, instructions, requires } = exported.skill;
    let skill = Skill { id, version: 1, name, description, instructions, requires, source: SkillSource::Imported { origin: origin.into() }, enabled: false };
    skill.validate()?;
    Ok(skill)
}

/// Where a host keeps the person's skills.
pub trait SkillStore {
    /// The skills file's bytes, or `None` when there is none yet.
    fn load(&self) -> Result<Option<Vec<u8>>, Error>;
    fn save(&self, bytes: &[u8]) -> Result<(), Error>;
    /// Held while a change reads, edits and writes the skills. Busy while another holds it.
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        Ok(Box::new(()))
    }
}

/// Skills in memory: tests, and the browser, where they last for the session.
#[derive(Clone, Default)]
pub struct MemorySkills(Rc<RefCell<Option<Vec<u8>>>>);

impl SkillStore for MemorySkills {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        *self.0.try_borrow_mut().map_err(|_| busy())? = Some(bytes.to_vec());
        Ok(())
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "your skills are busy; try again")
}

/// The skills as `skills.json` in a folder, replaced atomically so a crash or a full disk keeps
/// the last whole file; `.skills.lock` beside it orders changes between processes.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileSkills {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileSkills {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SkillStore for FileSkills {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        use std::io::Read;
        let unreadable = || Error::new(ErrorKind::Internal, "your skills can't be read");
        let file = match std::fs::File::open(self.dir.join("skills.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(unreadable()),
        };
        let mut bytes = Vec::new();
        let cap = u64::try_from(MAX_SKILLS_FILE_BYTES).unwrap_or(u64::MAX).saturating_add(1);
        file.take(cap).read_to_end(&mut bytes).map_err(|_| unreadable())?;
        Ok(Some(bytes))
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "your skills' folder can't be made"))?;
        craft_atomic::atomic_write(&self.dir.join("skills.json"), bytes)
            .map_err(|_| Error::new(ErrorKind::Internal, "your skills couldn't be saved (is the disk full?)"))
    }
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "your skills' folder can't be made"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(".skills.lock"))
            .map_err(|_| Error::new(ErrorKind::Internal, "your skills' lock can't be opened"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => busy(),
            std::fs::TryLockError::Error(_) => Error::new(ErrorKind::Internal, "your skills' lock failed"),
        })?;
        Ok(Box::new(file))
    }
}

/// The person's skills, read fresh each time, for the `skill.*` commands.
#[derive(Clone)]
pub struct UserSkills(Rc<dyn SkillStore>);

impl UserSkills {
    pub fn read(&self) -> Result<Skills, Error> {
        read(self.0.as_ref())
    }
}

fn read(store: &dyn SkillStore) -> Result<Skills, Error> {
    store.load()?.map_or_else(|| Ok(Skills::new()), |bytes| from_bytes(&bytes))
}

/// A skill changed since it was read.
pub fn stale(id: SkillId) -> Error {
    Error::new(ErrorKind::Conflict, format!("skill {id} changed since you read it; read it again and redo your edit"))
}

/// What a command that put `skill` in `scope` answers, whichever scope keeps it.
pub fn changed(skill: &Skill, scope: &str) -> Json {
    json!({"id": skill.id, "scope": scope, "version": skill.version, "name": skill.name, "enabled": skill.enabled})
}

/// One name per skill in a scope.
pub fn name_taken(skills: &Skills, skill: &Skill) -> Result<(), Error> {
    let key = skill_name_key(&skill.name);
    if skills.values().any(|other| other.id != skill.id && other.key() == key) {
        return Err(Error::new(ErrorKind::Conflict, "another skill already has that name"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    /// Adds `skill` (its JSON), or replaces the one with its id when that is at `expect`.
    Put {
        skill: String,
        expect: Option<u32>,
    },
    Remove {
        id: SkillId,
        expect: Option<u32>,
    },
}

/// Answers one `skills.store` request against `store`.
pub fn handle(store: &dyn SkillStore, request: &Json) -> Result<Json, Error> {
    let request: Request = serde_json::from_value(request.clone()).map_err(|_| invalid("malformed skills request"))?;
    let _lock = store.lock()?;
    let mut skills = read(store)?;
    let result = match request {
        Request::Put { skill, expect } => {
            if skill.len() > MAX_SKILL_EXPORT_BYTES {
                return Err(invalid("that skill is too large"));
            }
            let skill: Skill = serde_json::from_str(&skill).map_err(|_| invalid("malformed skill"))?;
            skill.validate()?;
            if skills.get(&skill.id).map(|old| old.version) != expect {
                return Err(stale(skill.id));
            }
            name_taken(&skills, &skill)?;
            let result = changed(&skill, "user");
            skills.insert(skill.id, skill);
            result
        }
        Request::Remove { id, expect } => {
            let removed = skills.get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "you have no skill with that id"))?;
            if expect.is_some_and(|expect| expect != removed.version) {
                return Err(stale(id));
            }
            skills.remove(&id);
            json!({"id": id, "scope": "user", "removed": true})
        }
    };
    validate_skills(&skills)?;
    store.save(&to_bytes(&skills)?)?;
    Ok(result)
}

/// Gives `session` the person's skills in `store`: what the `skill.*` commands read, and the
/// `skills.store` service that changes them.
pub fn install(session: &mut Session, store: Rc<dyn SkillStore>) -> Result<(), Error> {
    let service = store.clone();
    session.register_host_service(SKILLS_SERVICE, move |request| handle(service.as_ref(), request))?;
    session.swap_resource(Some(UserSkills(store)));
    Ok(())
}
