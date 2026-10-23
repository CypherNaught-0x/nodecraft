## 2026-10-23 · A spending cap per project, kept in the shared spending log (#847)

- **What a charge records.** Every `UsageEntry` names the project its run started in
  (`project`, omitted when there is none). The run host's log stays one file shared by every
  project, and keeps each project's own total beside the overall and per-session ones. Charges
  recorded before this have no project. They count towards the total only, so a project's
  spending starts from nothing the first time this build charges it.
- **Four caps.** `[budgets] project_usd_cents` caps the open project (`Budgets::this_project`,
  refused as "the project budget"). `total_usd_cents` stays the cap on everything the log
  records (`Budgets::project`, "the total budget"). The Budgets panel shows both, and what each
  has spent.
- **Which project.** An authorized run binds its document's id into each job's price, and the job
  is held to that project's cap when it reaches the provider, even if another project has been
  opened meanwhile. Quotes read the open document's id from the usage snapshot each time, so a
  project opened by a command (not just the File menu) is quoted against its own spending. The
  desktop app also re-installs the budget after a file opens. A job resumed after a restart is
  held to the project its journal entry names. `nodecraft-cli` binds its open project's id too,
  but keeps its log in memory with no caps.
- **New projects get their own id.** The desktop starter project used one fixed id, so every new,
  unsaved project would have shared one cap. It now draws a random id, and falls back to the
  fixed one only when no random source is available. Projects saved before this build, and
  Save As copies, keep the id they have, so those made from the old starter share one total
  until they are given new ids (#879). That is over-restrictive, never overspending. Since a
  crashed unsaved project now has its own id, its jobs resume only once it is recovered, not
  into whatever new project the next launch starts.
- **A bounded number of project totals.** At most `MAX_PROJECTS` (4,096) projects keep a total.
  Past that, the least recently charged project's total is forgotten (on saving too, so a cost reported for a new project at the limit can't make the file unreadable): its charges still count in
  the overall total, but its own total starts again from nothing. That is the documented
  boundary, and it only matters for someone with thousands of charged projects. Settling or
  releasing a charge whose project total was forgotten takes nothing from it.
- **Reading a log back.** A saved project total must not be larger than the overall total. If a
  total is smaller than its listed charges, it is raised to cover them, which never loosens a cap.
  The log format adds fields, so an older build refuses a log this one wrote (`deny_unknown_fields`),
  as it does for every earlier field added to the log. The CSV export adds a trailing `project`
  column, empty for charges without one; the quarantine export is unchanged.
