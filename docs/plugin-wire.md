# Plugin package and wire contract (protocol 1)

Phase A of the [plugin RFC](plugins-rfc.md) (#171): the identities, file formats and messages a
plugin package and the host exchange, frozen by fixtures. It is implemented by the standalone
`nodecraft-plugin` crate (`crates/plugin`), which decodes and checks bytes and nothing else: no
I/O, no installation, no runtime, no guest execution. Another implementation conforms when it
gives the same answers on [the fixtures](../crates/plugin/fixtures/README.md).

Every rejection carries a stable error code (below) and a location (a field, an index or a
package file name); field values, keys and text never appear in an error. Every limit is a hard ceiling checked before anything sized by input is allocated.

## JSON profile

All contract JSON is read under one profile, so two readers can never disagree about a document:

- UTF-8 only, no byte-order mark; whitespace is space, tab, CR and LF.
- Object keys unique (`duplicate_key`); unknown fields are errors (`unknown_field`).
- Nesting at most 32 (`too_deep`); strings, arrays, objects and the whole input bounded
  (`too_large`).
- Numbers are integers in ±(2^53−1); no fraction, exponent or `-0` (`bad_number`). Values that need
  the full `u64` range (revisions, epochs, byte counts) are **canonical decimal strings**: `"0"`
  or digits without a leading zero, at most 2^64−1.
- Strings: escaped control characters only, no lone surrogates (`bad_string`).
- Other numbers (fractions, exponents, `-0`, wider integers) appear only in opaque plugin data
  (see Preservation), kept as written; they have no canonical form in protocol 1, so parameter
  values that need fractions use the descriptor contract's (#162) string forms.

**Canonical JSON**: keys sorted by their UTF-8 bytes; no insignificant whitespace; integers in
shortest decimal; in strings only `"`, `\` and U+0000–U+001F are escaped (`\b \f \n \r \t`,
otherwise `\u00xx` in lowercase hex), every other code point is written as UTF-8 and never
normalised.

## Names

| Name | Grammar | Example |
|---|---|---|
| Package ID | 3–6 labels `[a-z][a-z0-9_]{0,23}`, dot-separated, ≤ 80 bytes | `io.example.textpack` |
| Entry ID | 1–2 segments `[a-z][a-z0-9_]{0,14}` | `normalize`, `text.normalize` |
| Registered ID | `plugin.<package>.<entry>`, ≤ 128 bytes (≤ 119 by the above) | `plugin.io.example.textpack.normalize` |
| Host ID (data binding) | dotted `[a-z0-9_]+`, ≤ 128 bytes, never under `plugin.` | `color.palette` |
| Token (job, request, handle, key ID, role) | `[a-z0-9][a-z0-9_-]{0,63}` | `job-7` |
| Version | `MAJOR.MINOR.PATCH`, decimals without leading zeros, no suffixes | `1.2.0` |

Nothing is rewritten to fit: a hyphen in a package label is `bad_id`, not an underscore.
Registered IDs are valid engine command IDs and node kinds as they are (checked by an engine
test). Package IDs that are prefixes of each other can spell the same registered ID
(`io.example.text` + `pack.x` and `io.example.text.pack` + `x`); registration refuses the second
whole, so install order never decides (`collision`).

## Package

A package holds `manifest.json`, its descriptors, data or module, its license files,
`inventory.json` and optionally `signature.json`.

**Manifest** (≤ 256 KiB, bytes already canonical, else `not_canonical`):

| Field | |
|---|---|
| `manifest_version` | `1` |
| `package_id`, `version` | as above |
| `host_protocol` | `{major, min_minor, max_minor}` |
| `descriptor_contract` | `{major, minor}` of the #162 contract its descriptors use |
| `publisher_key_id` | optional token |
| `license` | SPDX expression, ≤ 128 ASCII bytes |
| `runtime` | `{"kind":"data"}` or `{"kind":"rust_wasm","module":<path>}` |
| `dependencies` | ≤ 16 `{package_id, version, digest}` exact locks; not itself, not twice |
| `capabilities` | `{required, optional}`: `asset.read`, `asset.write`, `network`, `filesystem`, `process`, `gpu`, `secret`; an unknown required one is `unsupported_feature`, an unknown optional one is kept and ignored; a data package requires none (`incompatible`) |
| `features` | optional `{required, optional}` protocol features, dotted lowercase |
| `registrations` | ≤ 256 `{id, kind, descriptor, binding?}`; kinds `action`, `node_kind`, `provider`, `catalog`, `technique`; `binding` is the host implementation a data package configures (data packages only, else `incompatible`) |

Every file the manifest names (its module and descriptors) must be listed in the inventory, so
all of it is under the digest.

**Inventory** lists every file except `inventory.json` and `signature.json` (naming either, in
any letter case, anywhere a path is expected is `bad_path`), always including `manifest.json`,
in byte order of the paths, each once (`not_canonical`):
`{"files":[{"blake3":<64 hex>,"bytes":<decimal>,"path":<path>}],"inventory_version":1}`. At most
1,024 files and 128 MiB together. Paths are ASCII `[A-Za-z0-9._-]` segments joined by `/`, ≤ 128
bytes and 8 segments; no empty, `.` or `..` segment, none ending in `.`, no Windows device name
(`con`, `nul`, `com0`–`com9`, `lpt0`–`lpt9`, …), and no two paths equal after ASCII case folding (`collision`). A file
present but unlisted, or listed but missing, fails the listing check.

**Package digest**: BLAKE3 of `nodecraft-plugin-v1\0` followed by the canonical inventory JSON;
on the wire `blake3:` plus 64 lowercase hex digits (the prefix isn't hashed). Any changed byte
in any file changes it; the digest, not the version, identifies what runs.

**Signature** (`signature.json`): `{"algorithm":"ed25519","key_id","package_digest","signature":<128 hex>,"signature_version":1}`.
Ed25519 (RFC 8032), verified strictly, over `nodecraft-plugin-signature-v1\0` followed by the 32
digest bytes. When the manifest names a `publisher_key_id`, only that key may have signed. **Trust file**: `{"keys":[{"algorithm":"ed25519","key_id","provenance","public_key":<64 hex>,"status":"trusted"|"revoked"}],"trust_version":1}`,
at most 256 keys, IDs and public keys unique, provenance one line of ≤ 256 bytes. Outcomes:
unsigned; verified by a trusted key; signed by an unknown key; or an error for a revoked key,
a bad signature (`bad_signature`) or a signature for another digest (`bad_digest`). A digest
proves integrity and a signature proves who signed; neither proves safety or licensing. There is
no registry, network lookup or rotation chain; a revoked key stays listed so re-importing it
can't silently trust it again.

## Frames and messages

A frame is a 4-byte big-endian length and that many bytes of canonical JSON, 1 to 1 MiB; a
payload in any other form is `not_canonical`, so every message has exactly one encoding. The
length is checked at the header; a bad one poisons the stream. A reader takes frames as they
complete and holds at most two frames' worth of untaken bytes. Every message has `"protocol":1`
and a `type`:

| Type | Direction | Fields |
|---|---|---|
| `initialize` | host → guest | `protocol_minor`, `descriptor_major`, `descriptor_minor`, `features` |
| `initialized` | guest → host | `protocol_minor`, `features`, `registrations` (registered IDs) |
| `invoke` | host → guest | `request_id`, `job_id`, `command_id`, `package_digest`, `expected_revision`, `grant_epoch` (decimals), `params` (≤ 256, keyed by registered parameter IDs), `inputs` (≤ 64 `{role, handle, bytes}`), `output_budget` `{bytes, items ≤ 64}`, `idempotency_key` (1–128 printable ASCII) |
| `accept` | guest → host | `job_id` |
| `progress` | guest → host | `job_id`, `value` 0–1000, optional `note` (≤ 1 KiB) |
| `result` | guest → host | `job_id`, `grant_epoch`, `outputs` (≤ 64), `proposed_effect` `{"kind":"attach_output","target_node":<token>}` |
| `error` | guest → host | `job_id`, `code` (`InvalidInput`, `UnsupportedCapability`, `ResourceLimit`, `Cancelled`, `Conflict`, `Internal`), `field_ids` (registered IDs, each once; may be empty), `message` (≤ 4 KiB, no control characters but newline and tab) |
| `cancel` | host → guest | `job_id` |
| `cancel_ack` | guest → host | `job_id` |

Byte counts in streams are claims to check against the stream, never sizes to allocate. Handles,
registrations and field IDs are listed once per message; feature names are dot-separated parts
of `[a-z][a-z0-9_]*`, as in manifests. No message carries a path, URL, secret or engine object, and a guest
proposes only `attach_output` to a host-bound token; the host builds any document change.

**Job order** (checked for every guest message): `invoke` → `accept` → `progress`* (never going
back) → exactly one `result` (for the invoking grant epoch) or `error`; an `error` may also refuse
before `accept`. A host `cancel` is terminal at once: anything the guest sends for that job is
discarded, and `cancel_ack` closes it. Anything else is `sequence`. A result with more outputs or
bytes than the invocation's `output_budget` is `too_large`. Cancelled jobs don't count against
the 64 running jobs; the host forgets one when it resets the sandbox after the grace period, and
at most 64 cancelled jobs wait for an acknowledgement.

## Negotiation and locks

Protocol majors must be equal; the minor is the highest both ranges include. Descriptor contract
majors must be equal and the package's minor no newer than the host's. Every required feature
must be known to the host (`unsupported_feature`); unknown optional ones drop out. Host today:
protocol 1.0, descriptor contract 1.0, no optional features.

Dependency locks are exact version and digest, satisfied only by installed packages
(`dependency_missing`); dependencies are data packages, at most 8 deep (`dependency_rejected`),
acyclic (`dependency_cycle`); one version of a package at a time. No resolver, no fetching.

## Preservation (#54)

A project opened without a plugin keeps everything about its nodes:

- a **requirement** per package: ID, version, digest, protocol major, descriptor contract and the
  registered IDs it uses (all in that package), never grants, paths or credentials;
- **descriptor snapshots**: canonical descriptor bytes (≤ 64 KiB) named by their BLAKE3, shared by
  every node that uses them;
- **opaque config** per node (≤ 64 KiB): checked to be bounded JSON with unique keys (any valid
  number literal up to 256 characters, including `-0`, `1e400` and wide integers), then kept
  byte for byte. In the project envelope it is a JSON *string* holding the
  original text, `{"payload":"…","version":N}`, so the envelope can be re-serialised (the format
  crate re-emits projects with serde) without touching the payload's key order, spacing or number
  spellings.

All plugin data in one project is bounded at 8 MiB, snapshots counted once per digest.
Wiring these into `.ncraft` and the missing-plugin placeholder card is phase B/D work.

## Error codes

`too_large`, `too_deep`, `syntax`, `duplicate_key`, `unknown_field`, `missing_field`,
`wrong_type`, `bad_number`, `bad_string`, `bad_id`, `bad_version`, `bad_path`, `bad_digest`,
`collision`, `incompatible`, `unsupported_feature`, `dependency_missing`, `dependency_cycle`,
`dependency_rejected`, `bad_signature`, `sequence`, `not_canonical`.
