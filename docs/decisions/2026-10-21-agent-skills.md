## 2026-10-21 · Agent skills are versioned guidance text in two scopes (#168)

- **A skill is text, not code.** A skill (playbook) has a stable `SkillId`, a version, a name,
  a description, instructions, and the commands, Techniques (optionally pinned to a version) and
  host capabilities it declares that it needs. It has no hooks, scripts, credentials or
  parameters that run. A Technique stays the executable graph; a skill is guidance that may tell
  the agent to place one. Reading a skill (`skill.read`) runs nothing.
- **Two scopes, and the project's wins.** Project skills are in the document (`Project.skills`).
  Edits to them are `PutSkill`/`RemoveSkill` changesets, so they are undoable, journalled and
  saved; only format v3 carries them, so the v1 and v2 writers and readers refuse them. The
  person's own skills are in `skills.json` in the settings folder, beside the technique library
  (`skills/`). Changes are atomic and run under a cross-process lock, through the `skills.store`
  host service. The browser keeps them in memory for the session. A project skill shadows a
  personal skill with the same name: both are listed, and the personal one is marked `shadowed`.
  Names are unique within a scope, compared trimmed and case-folded. Ids are unique across both
  scopes. A shared library comes later through #119/#121 as a third scope; nothing here assumes a
  hosted service.
- **Versions pin reads and refuse lost updates.** Every edit raises the version by one.
  `skill.edit`, `skill.enable` and `skill.delete` carry the version they read. If the skill has
  changed since, the edit is refused as a Conflict. The store checks again under its lock. So does
  `skill.read {version}`.
- **Missing requirements are reported, never installed.** `skill.list` names the required
  commands this session lacks, Techniques neither the project nor the library holds (at the pinned
  version), and capabilities the host doesn't serve.
- **Imports are untrusted and start off.** `skill.export` writes only the portable part
  (`nodecraft.skill` format, version 1: name, description, instructions, requirements). It has no
  ids, version, trust or enabled flag. `skill.import` refuses unknown fields, other formats and
  files over 256 KiB, and reports a newer format version as Unsupported. An imported skill is
  marked `imported` with the origin the person gave, and is disabled. Reading it adds a note that
  its text is untrusted. Every skill's note says that guidance cannot override the person's
  request, approvals, budgets or cost confirmations. These are still enforced by the commands the
  agent runs, not by the skill.
- **The person decides trust.** Agents (MCP and the in-canvas agent) may list, read, create, edit,
  delete and export skills. Only a person runs `skill.import` (what untrusted text comes in) and
  `skill.enable` (which skills are offered to the agent). A skill made by `skill.create` starts
  off, and a `skill.edit` that changes its name, description, instructions or requirements turns
  it off again, so an agent can't write itself an enabled playbook. `skill.edit` cannot change
  `enabled`. Personal skills are outside undo, so a mistaken agent edit is recovered by editing
  again. The version check keeps it from overwriting an edit it didn't see.
- **A project's skills are untrusted text.** They arrive with the project file, and anyone may
  have written that file, including its `source` and `enabled` fields. So `skill.list` and
  `skill.read` mark every project skill untrusted, and `skill.read` adds the untrusted note.
  Before part 2 offers a project's enabled skills to the agent, the person approves them on this
  machine, per project.
- **Ids can clash across scopes.** A project file may carry a skill whose id matches one of the
  person's. Without `scope`, the commands act on the project's skill. `skill.read`, `edit`,
  `enable`, `delete` and `export` take an optional `scope` to pick the person's. If the person's
  store can't be read, `skill.list` still lists the project's skills and reports the error as
  `user_error`.
- **Versions can repeat after undo.** Undoing a project skill edit brings back the earlier
  version number with the earlier text, and an edit pinned to the undone version is refused. A
  new edit after that undo takes the undone number again with different text, so a reader that
  pinned the undone version would match it (ABA). We accept this for now: it takes an undo and a
  fresh edit in between, and both are visible in the project's history.
- **Later (#168 part 2):** offering enabled skills to the in-canvas agent, the per-machine
  approval of a project's skills, explicit activation, and recording which skill and version
  guided a run, without its input text. UI for import, enable and delete comes with it.
