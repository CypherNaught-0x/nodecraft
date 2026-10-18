# 2026-10-17 · Scoped live control navigation (#262)

- MCP and control share command admission and result envelopes. A trusted frontend host supplies
  fresh availability and dispatch through Session or the supported live navigation bridge.
  Navigation capabilities are scoped to each call and restored on success or failure; the control
  protocol never installs a global capability or duplicates the automation permission lists.
