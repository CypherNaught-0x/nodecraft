## 2026-10-23 · Several ComfyUI servers, each its own provider instance (#864)

**Settings.** `[providers] comfyui_url` became a list, `[[providers.comfyui]]` (`name`, `url`,
optional `max_jobs`), plus `comfyui_default` naming one. Settings schema version 2 migrates the
old address into one server named after its host and port, and makes it the default; a
file with no `schema_version` that still holds `comfyui_url` is read the same way. Names are
unique, 1–64 characters, without control characters; at most 32 servers are kept.

**Routing.** A server's adapter is kept in the provider setup under `comfyui@<name>`. It is bound as
the `comfyui` adapter with a named instance derived from the server's name
(`provider_setup::comfy_instance`), so each server gets its own scheduler lane, and a job
restored after a restart is matched back to its server by that instance. The desktop app and
`nodecraft-cli` (its runs and the MCP server's estimates) route the same way. A card picks a
server with `comfy.server.use`, which stores the name in the card's config (`comfyui.server`). With no
choice, or a name that is no longer in settings, the card follows the default server. A legacy
job with no instance resumes on the default. A job whose server was removed stays waiting rather
than being sent elsewhere.

The name is kept in the card, not a server id, so a shared project still makes sense on another
machine whose servers have the same names, and falls back to that machine's default otherwise.

**Jobs at once.** `max_jobs` is carried on the routing binding and applied as that instance's
scheduler limit for each run (`AuthorizedRun::lane_limits`). It is not a cap across concurrent
runs. It is 1–16 (the scheduler runs at most 16 jobs in all; unset, a server takes the usual 4),
and a hand-edited value out of range is clamped rather than resetting `[providers]`. A
single-GPU box is usually set to 1.

**Checks and the node list.** `comfy.nodes.refresh {server}` checks one server. Every server's last
check is kept for the panel and `comfy.nodes.status.servers`, but only the default's node list is
installed for importing workflows. Changing the default drops the list until the new default is
read. Editing or removing a server drops that server's adapter, report and running check; other
servers' adapters stay built, though builds in flight are asked again. Checking a name that isn't
a server is refused without keeping a report.
