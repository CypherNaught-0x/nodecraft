## 2026-10-23 · Headless provider estimates (#683)

- **The existing command and run host own prices.** CLI runners register
  `RunHost::register_estimates`. `quote --estimate` and `run --estimate` explicitly dispatch
  `run.estimate`, pump until its own request is answered, then obtain a new `run.quote`.
  Asking sends the job's inputs to the provider, including image uploads; it never submits a
  generation or registers a charge. Plain `quote`/`run` keep their existing behavior.
- **A bounded reply names its request.** Native headless estimates return the accepted request
  ID and `status: estimated` with `canvas`, `quoted` and `unquoted`, or `status: not_estimated`
  with `reason`. The engine ends incomplete requests after 60 seconds; a defensive 62-second
  host deadline also bounds the price wait after adapter/keychain setup. OS keychain prompts
  can wait for the person independently of that deadline. A failed or timed-out provider quote stays unknown rather
  than becoming free, and starting a run still requires the normal price token and consent.
- **MCP supplies a frontend host through the shared protocol.** `McpServer::with_host` takes
  the existing `CommandHost` interface. The standalone CLI's MCP frontend owns an estimates-only
  run host and routes the requested selected/field cards, or requested upstream closure, before
  dispatching. It does not install the paid run service. Other commands retain engine dispatch,
  schema checks, access checks and panic guards. The desktop bridge keeps its existing asynchronous
  accepted-request contract.
- **Idle MCP has no frame pump.** Each later command pumps the estimates-only host to expire
  old prices before dispatch; ten-minute quote validity therefore does not depend on another
  estimate being requested. A caller refreshes a price explicitly with another `run.estimate`.
  Quotes are process/session-local, never persisted in the project file.
