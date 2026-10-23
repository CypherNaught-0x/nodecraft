## 2026-10-23 · The ElevenLabs voice field lists the account's voices (#670)

- **`elevenlabs.voices` is an option source**, fixed in `crates/engine/src/option_fetch.rs` like
  Higgsfield's lists. The four models that take a voice (Eleven v3, Multilingual v2, Flash v2.5
  and the voice changer) gained a parameter contract whose only rule gives `voice_id` that list.
  Their parameters are otherwise unchanged, so a run sends the same request as before.
- **The premade choice stays.** **Voice** keeps its ten premade voices, which work with no fetch
  and offline. **Your voice** overrides it, as the typed id did; once fetched, it is a picker of
  every voice the account can use, premade ones included.
- **`GET /v2/voices` is read in full, within caps.** It is paged by token, so the adapter follows
  `next_page_token` while `has_more` holds, up to 50 pages of 100, and refuses an account with
  more rather than listing part of it silently. A page that says there is more but gives no
  token, or one longer than 1 KiB, is refused rather than guessed at.
- **Only voices a run would accept are listed.** The run refuses a voice id that isn't ASCII
  letters and digits (it goes into the request path), so the list skips and counts such entries
  rather than offering a choice that fails later.
