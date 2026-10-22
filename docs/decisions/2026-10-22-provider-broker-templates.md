## 2026-10-22 · Plugin providers are templates the host sends (#174)

- **A template, not code.** A `provider_operations` package registers providers whose descriptors
  are declarative request templates (`docs/plugin-wire.md`). The host's broker builds every
  request from the template and the job, and adds the credential itself. The package never holds
  a key, opens a connection or picks a destination.
- **Destinations are fixed by the template.** A template names one HTTPS API origin and up to four
  output origins, each a public DNS name. A job or a reply can't change them: paths take only a
  listed model and a checked job id, body placeholders fill whole values, and output links off
  those origins are refused. The template's origins are what a grant of its network capability
  will cover.
- **Credentials go only where providers expect them.** The template picks a placement from a
  fixed host list (bearer, `x-api-key`, …) and names no header of its own. The host attaches the
  credential only to a request for the template's API origin, never an output origin, as a secret
  header that doesn't follow a redirect to another origin or reach a log. A browser `fetch` keeps
  custom headers across redirects, so the broker refuses to run in the browser.
- **A key belongs to a package and an origin.** A key the user enters for a plugin provider is
  stored for that package and API origin, never shared with a built-in provider or another
  package, so a template pointed at another host can't receive a key meant elsewhere.
- **Never free.** Every model declares a positive per-job price in exact money, so a template
  can't present a paid request as free. That price is the publisher's word: the host's quote never
  goes below it, and budgets and confirmation treat it as a floor, not a price the host trusts.
- **No idempotency, no automatic retry.** Without an idempotency header, a submit whose reply was
  lost is never resent automatically; it is marked as possibly charged.
- **Nothing is granted without its capabilities.** Grants don't record capabilities yet, so the
  store refuses to grant or enable a package that requires one, even under a grant written
  earlier. A provider operations package installs, but runs nothing until grants are scoped.
- **Delivered in parts.** This first part sets the template contract and its checks. Scoped grants
  (capabilities, origins, permission deltas, epoch revocation) and the broker adapter (keychain,
  DNS checks, quotes, reservations, reconcile) follow under #174. Nothing reaches the network until
  both land and are reviewed.
