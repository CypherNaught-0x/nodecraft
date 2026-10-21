//! Agent skills (#168): playbooks that teach the agent a workflow in words, using existing
//! commands and Techniques (`docs/decisions/2026-10-21-agent-skills.md`). A skill is guidance,
//! never something that runs: it holds text and names what it needs, and nothing else. A project
//! keeps its own skills in the document, so they travel with the file and every change undoes.
use nodecraft_types::{Error, SkillId, TechniqueId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::validate::{invalid, text};

/// Most bytes in a skill's name.
pub const MAX_SKILL_NAME_BYTES: usize = 128;
/// Most bytes in a skill's description.
pub const MAX_SKILL_DESCRIPTION_BYTES: usize = 1024;
/// Most bytes in a skill's instructions.
pub const MAX_SKILL_INSTRUCTIONS_BYTES: usize = 32 * 1024;
/// Most commands, Techniques or capabilities one skill names as needed, of each.
pub const MAX_SKILL_REQUIREMENTS: usize = 64;
/// Most bytes in where an imported skill came from.
pub const MAX_SKILL_ORIGIN_BYTES: usize = 256;
/// Most skills one project, or the user's library, holds.
pub const MAX_SKILLS: usize = 256;
const MAX_ID_BYTES: usize = 128;

/// A playbook: what it is for, what to do, and what it needs to do it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    pub id: SkillId,
    /// 1 when made, one more for each edit, so a reader can pin what it read.
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Requires::is_empty")]
    pub requires: Requires,
    #[serde(default)]
    pub source: Source,
    /// A disabled skill is listed but never offered to the agent; imported skills start disabled.
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

/// What a skill needs: commands by id, Techniques by id (and version, when it was written for
/// one), and host capabilities by service id (`library.store`). Only declared: listing a skill
/// reports which are missing, nothing installs them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub techniques: Vec<TechniqueRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl Requires {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.techniques.is_empty() && self.capabilities.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechniqueRef {
    pub id: TechniqueId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

/// Who wrote a skill, as far as NodeCraft knows: written here, or imported from a file whose
/// text nobody here checked. Shown beside the skill; an imported skill's text is untrusted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    #[default]
    Written,
    Imported {
        /// Where it came from as the person importing it named it (a file name, an author);
        /// informational only.
        #[serde(default)]
        origin: String,
    },
}

impl Skill {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version == 0 {
            return Err(invalid("a skill's version starts at 1"));
        }
        if self.name.trim().is_empty() {
            return Err(invalid("a skill needs a name"));
        }
        text(&self.name, MAX_SKILL_NAME_BYTES)?;
        if self.name.chars().any(char::is_control) {
            return Err(invalid("a skill's name is one line"));
        }
        text(&self.description, MAX_SKILL_DESCRIPTION_BYTES)?;
        if self.instructions.trim().is_empty() {
            return Err(invalid("a skill needs instructions"));
        }
        text(&self.instructions, MAX_SKILL_INSTRUCTIONS_BYTES)?;
        let Requires { commands, techniques, capabilities } = &self.requires;
        if [commands.len(), techniques.len(), capabilities.len()].into_iter().any(|n| n > MAX_SKILL_REQUIREMENTS) {
            return Err(invalid("a skill names too many requirements"));
        }
        if !commands.iter().chain(capabilities).all(|id| dotted(id)) {
            return Err(invalid("a skill's required command or capability isn't an id like node.add"));
        }
        if techniques.iter().any(|technique| technique.version == Some(0)) {
            return Err(invalid("a technique's version starts at 1"));
        }
        if let Source::Imported { origin } = &self.source {
            text(origin, MAX_SKILL_ORIGIN_BYTES)?;
        }
        Ok(())
    }

    /// The name as two skills are told apart by: trimmed and case-folded.
    pub fn key(&self) -> String {
        name_key(&self.name)
    }
}

/// `name` as skill names compare: trimmed and case-folded.
pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A command or service id: lowercase words joined by dots.
fn dotted(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
}

/// A set of skills, as a project or the user keeps them: each valid, under its own id, and no
/// two with the same name.
pub fn validate_skills(skills: &BTreeMap<SkillId, Skill>) -> Result<(), Error> {
    if skills.len() > MAX_SKILLS {
        return Err(invalid("too many skills"));
    }
    let mut names = BTreeSet::new();
    for (id, skill) in skills {
        if *id != skill.id {
            return Err(invalid("skill map key does not match its id"));
        }
        skill.validate()?;
        if !names.insert(skill.key()) {
            return Err(invalid("two skills share a name"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill() -> Skill {
        Skill {
            id: SkillId::from_bytes([1; 16]),
            version: 1,
            name: "Product shots".into(),
            description: String::new(),
            instructions: "Place a Text node, then an image node.".into(),
            requires: Requires::default(),
            source: Source::Written,
            enabled: true,
        }
    }

    #[test]
    fn bounds_hold() {
        assert!(skill().validate().is_ok());
        for broken in [
            Skill { version: 0, ..skill() },
            Skill { name: " ".into(), ..skill() },
            Skill { name: "a\nb".into(), ..skill() },
            Skill { name: "x".repeat(MAX_SKILL_NAME_BYTES + 1), ..skill() },
            Skill { instructions: String::new(), ..skill() },
            Skill { instructions: "x".repeat(MAX_SKILL_INSTRUCTIONS_BYTES + 1), ..skill() },
            Skill { requires: Requires { commands: vec!["Node Add".into()], ..Requires::default() }, ..skill() },
            Skill { requires: Requires { commands: vec!["node.add".into(); MAX_SKILL_REQUIREMENTS + 1], ..Requires::default() }, ..skill() },
            Skill {
                requires: Requires { techniques: vec![TechniqueRef { id: TechniqueId::from_bytes([2; 16]), version: Some(0) }], ..Requires::default() },
                ..skill()
            },
            Skill { source: Source::Imported { origin: "x".repeat(MAX_SKILL_ORIGIN_BYTES + 1) }, ..skill() },
        ] {
            assert!(broken.validate().is_err(), "{broken:?}");
        }
    }

    #[test]
    fn round_trips_and_refuses_unknown_fields() {
        let skill = Skill { source: Source::Imported { origin: "team.json".into() }, enabled: false, ..skill() };
        let json = serde_json::to_value(&skill).unwrap();
        assert_eq!(serde_json::from_value::<Skill>(json.clone()).unwrap(), skill);
        let mut extra = json;
        extra["run"] = serde_json::json!("rm -rf /");
        assert!(serde_json::from_value::<Skill>(extra).is_err());
    }
}
