## 2026-10-22 · Veo runs on Google Cloud with a service account kept compact in the keychain (#742)

- **A service account, not an API key or the gcloud CLI.** Vertex AI's Veo endpoints take OAuth
  access tokens. A service account's JSON key can mint them with no browser sign-in and no external
  tool, on every platform including the web build. NodeCraft signs the JWT itself (RS256, scope
  `cloud-platform`) and exchanges it at Google's token endpoint. Tokens are held in memory only.
  Other Google Cloud adapters (Google TTS, #21; Imagen) can reuse `providers::google_cloud`.
- **The keychain keeps only what signing needs.** A key file is about 2.3 KB. Windows Credential
  Manager holds about 1280 characters, and keys are capped at 1024 bytes everywhere. `keys.set`
  for the `google-cloud` account therefore takes the whole file. It checks the file and stores
  one line: `gsa1:<email>:<project>:<key id>:<p>:<q>`, about 450 characters for a 2048-bit key.
  The modulus and private exponent are rebuilt from the two primes. Nothing else from the file is
  needed: the token endpoint is fixed, and other endpoints are refused.
- **RSA comes from the `rsa` crate**, pinned exactly (MIT/Apache-2.0; RustCrypto). Signing is
  PKCS#1 v1.5, which is deterministic. It runs once an hour on the person's own machine, so the
  crate's known timing advisory gives a remote observer nothing to measure.
- **Region and project are fixed by the key.** Runs go to the key's project in `us-central1`, the
  region where every Veo 3.1 model is documented. A `location` setting can come later if anyone
  needs another region.
- **Veo's price has two dimensions, so the catalog gained `per_unit_by_choice`.** It prices
  `count` units (here the duration in seconds) at the price listed for a chosen value (here the
  resolution). Audio is always on, so the "with audio" column is the price. Seedance and Kling are
  priced the same way and can reuse it.
- **Only inline results are accepted.** Without `storageUri`, Vertex AI returns the video as
  base64 in the operation. A reply naming only a Cloud Storage object is refused as malformed
  rather than fetched with the account's token.
- **Open questions, until a live run settles them:** whether "/1 count" on the pricing page means
  per second, whether 4K is generally available, whether a filtered clip is billed, and whether
  Lite takes images on Vertex AI (it is text only here until then).
