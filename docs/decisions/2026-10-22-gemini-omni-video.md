## 2026-10-22 · Google video on the Gemini API is Omni Flash, polled as a file (#17)

- **Omni Flash, not Veo.** Every Veo model on the Gemini API is shut down: Veo 2 and 3.0 on
  2026-06-30, Veo 3.1, 3.1 Fast and 3.1 Lite on 2026-10-22
  ([deprecations](https://ai.google.dev/gemini-api/docs/deprecations)). Google names
  `gemini-omni-1.1-flash` as the replacement, so the `gemini-video` adapter runs it. Veo remains
  on Google Cloud; that adapter needs Cloud authentication and is tracked separately (#742).
- **One synchronous request, then a file to poll.** Omni Flash is called through the
  Interactions API with `store`, `background` and `stream` false, as Google recommends. It asks
  for `delivery: "uri"`, so the reply names a Files API file rather than carrying the video. The
  adapter returns that file id as the job handle, polls `GET files/{id}` until it is `ACTIVE`,
  and downloads `files/{id}:download?alt=media` with the key. A reply that carries the video
  inline anyway finishes at submit. The uri is only in the create reply (a later
  `GET interactions/{id}` returns inline data), so the handle is all a resumed run needs. The
  file is not deleted after download: Google expires files after two days, and deleting one
  could lose a paid video if the result isn't recorded before a crash.
- **The generate request is the paid step and is never replayed.** It is marked unknown
  before sending, a lost reply says the video may have been billed, and
  `retry_ambiguous_submit` is false. A failed download stays retryable because the file is
  still there. Cancel is unsupported: the video exists once the request returns.
- **No price before the run.** Omni Flash has no duration parameter (length follows the
  prompt), so the catalog prices it `by_usage` with token rates: 1.50 USD per million input
  tokens and 17.50 USD per million output tokens, the video rate. That is about 0.10 USD a
  second at 720p (5,792 tokens a second); the text output rate is lower, so it is an upper
  bound. Finite budgets refuse it as they do every by-usage model.
- **Offered:** text to video; a first frame with an optional last frame; one to three subject
  references; 16:9 or 9:16; 360p, 720p or 1080p. 4K is left out until its token rate is
  published, and so are video edit, extend and video references (multi-turn and video inputs
  come with a later issue).
