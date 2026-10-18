# 2026-10-17 · Provider foundation (#141)

- **`Provider` is dyn-compatible** (boxed futures, `Send` on native only) so the engine can hold
  a registry of `Box<dyn Provider>`; no `async-trait` dependency.
- **`exec` and `providers` don't know each other** (same layer). The engine implements
  `exec::driver::JobRunner` with `providers::run_to_completion`, wrapping exec's `CancelToken`
  and the runtime as `providers::CancelSignal` / `Clock`.
- **A job is submitted once.** Transient poll errors are retried in place. A run that fails after
  the provider accepted the job returns its handle (`RunFailure::handle`), and the engine's
  `jobs::ProviderRunner` keeps it, so the scheduler's retry *resumes* the job instead of
  submitting (and paying for) it again. A job past its deadline is `Expired`, which is never
  retried; a cancel the provider couldn't confirm says so ("may still run and be billed").
  `JobHandle` is serialisable so accepted jobs can be resumed after a restart.
- **Deadlines use real time** (`Clock::now_ms`, the app's monotonic clock), not the sum of sleeps.
- **`submit` may finish at once** (`Submission::Finished`) for synchronous APIs (chat, OpenAI
  Images, TTS), so their outputs don't get stashed in a handle.
- **HTTP through an `Http` trait** so adapters are tested against fixtures. The reqwest transport
  sends only `NodeCraft/<version>` as User-Agent, applies timeouts on every target, caps reply
  sizes while streaming, and keeps header values, URL credentials and queries out of `Debug`
  output and errors. It follows redirects itself: credential headers (the well-known ones plus
  any marked with `secret_header`) are dropped when a redirect leaves the origin, so CDN download
  redirects work without leaking keys. In the browser, `fetch` follows redirects and only drops
  `Authorization`; adapters there must not rely on custom key headers being dropped.
- **TLS:** the OS stack (`native-tls`) on Windows and macOS, rustls with aws-lc on Linux, `fetch`
  in the browser. aws-lc needs clang-cl for Windows ARM64, which the release builds.
- **Downloads are checked and hashed:** sniffed with `media::sniff` (an error page served as 200
  is refused), length-checked against `Content-Length`, and returned with their BLAKE3
  `ContentHash` for the asset store. Inline output bytes go through the same check.
