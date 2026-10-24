## 2026-10-23 · Spending caps are the person's settings, not a command (#201)

- **Where they live.** The per-call, session and total caps are the `[budgets]` section of
  `config.toml` (`per_call_usd_cents`, `session_usd_cents`, `total_usd_cents`), each unset by
  default and read with a ceiling of 100,000 USD. A cap the file holds that isn't a whole number
  of cents reads as 0, which blocks paid runs until it is set again; it never fails the section,
  which would lift every cap. The desktop app hands them to the run host at
  start and whenever the Budgets panel saves them; quotes and the jobs already running are checked
  against the new caps from then on. The browser keeps them for the page only, as it does every
  other setting.
- **No command changes them.** Everything else a person can do is a command, but a cap exists to
  stop spending the person didn't intend, including spending an agent, script or MCP client
  starts. If a command could raise a cap, whatever runs commands could lift its own limit. So the
  caps change only in the Budgets panel (or by editing the file), and a run they block says which
  cap and, in the app, offers "Change budgets…". `nodecraft-cli` keeps running without caps until
  it has a settings story of its own.
- **Dollars only, for now.** Caps are in US dollars, the currency every catalogue price uses
  today. A price in another currency is blocked under a cap, as `cost::check` already does.
- **"In total", not per project.** The run host's spending log is one file in the settings folder,
  shared by every project the app opens, and its entries name no project. So the third cap is on
  everything the log has recorded, across projects and sessions, and the panel says so. A real
  per-project cap needs the log to record which project each charge was for; that came later
  (`project_usd_cents`, see 2026-10-23-project-budgets).
