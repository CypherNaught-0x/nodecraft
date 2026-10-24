## 2026-10-23 · The broker sends plugin provider requests itself (#174)

- **One provider for every template.** `broker::Broker` is the `Provider` for a plugin's
  provider: it plans each request from the template, adds the credential, and sends it. The
  engine builds one per registered provider with the package's own key; nothing a package ships
  runs.
- **No redirects, anywhere.** Every request, to the API and to an output origin, goes out without
  following a redirect, and a `3xx` reply is an error. The origins a grant covered are the only
  ones reached; a service can't move a job, a key or a download elsewhere by answering `307`.
- **Outputs without the key, typed by their bytes.** Output links are fetched by the host without
  the credential, bounded, and sniffed, so an output origin that the publisher runs never sees the
  key and a template's `mime` is never trusted. One output holds at most 256 MiB and one job's
  outputs 512 MiB together. A download that isn't media, or that holds the key, is refused
  without quoting it; one that may work next time is retried, since the job is already billed.
- **An unknown service proves nothing.** Built-in adapters may treat a provider's documented
  refusals as proof that nothing was billed. A template carries no such list the host could check,
  so once the paid request is sent every failure is possibly billed. A lost reply is resent
  automatically only under the template's idempotency header.
- **Public addresses only.** A template's origin is a public DNS name, but its records are the
  publisher's. The desktop transport for plugin providers resolves names on its own resolver,
  refuses any name with a non-public address among its records (loopback, private, link-local,
  carrier-NAT, documentation, multicast, and IPv4 inside IPv6), and uses no proxy, which would
  resolve the name itself. A URL naming an address directly is checked the same way. Users behind a mandatory proxy can't use plugin providers yet.
- **The quote is the template's price.** It asks no one, and the template's price is a floor.
- **Not wired yet.** The engine doesn't build a broker yet. Snapshotting a granted package into
  the provider list, keychain accounts bound to package and origin, a revocation check right
  before the paid send, and reservation recovery follow as the last part of #174.
