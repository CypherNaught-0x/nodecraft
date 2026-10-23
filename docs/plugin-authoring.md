# Writing a plugin package

This guide covers building a `.ncraft-plugin` package, checking it, and installing it. The
contract itself (the JSON profile, names, the inventory and digest, negotiation and error codes)
is in [plugin-wire.md](plugin-wire.md), and the reasons behind it are in
[plugins-rfc.md](plugins-rfc.md). Two complete packages to start from are in
[`examples/plugins/`](../examples/plugins/).

## What a package can do today

| Runtime (`runtime.kind`) | What it registers | Runs code? | Desktop | Browser |
|---|---|---|---|---|
| `data` | catalog entries and descriptors for host features | no | yes | yes, for the session |
| `text_actions` | text actions written in NodeCraft's [expression language](text-expressions.md) | interpreted by the host, after a grant | yes | no |
| `provider_operations` | models served by an HTTP API the package describes ([templates](plugin-wire.md#provider-operation-templates-174)) | the host sends requests, after a grant | yes | no |
| `rust_wasm` | — | — | refused | refused |

Packages can't ship native or WebAssembly code yet. A `rust_wasm` package is refused at install,
with an "unsupported" error. Nothing in a package ever runs while it is packed, linted or
installed: there are no build, install or post-install hooks.

## The layout

A package is a folder with a `manifest.json` at its root, a licence file (`LICENSE*`, `COPYING*`
or `NOTICE*`), and the descriptors the manifest's registrations name:

```text
shout/
  manifest.json
  LICENSE
  actions/shout.json
```

The manifest may be written readably, with spacing and in any key order: `plugin pack` writes its
canonical form into the package. Every other file is packed byte for byte. Names that start with
`.` (`.git`, `.DS_Store`) are left out, and a link stops the pack, because a package holds only
regular files. The rules for paths (ASCII, at most 128 bytes and 8 segments, and no two that differ
only in case) are in [plugin-wire.md](plugin-wire.md#package).

`inventory.json` and `signature.json` are written by the package format. A folder holding either
one is refused.

### IDs

`package_id` is a reverse-DNS name you control (`io.github.you.palette`). Each registration's
`id` is local to the package: `shout` in `io.example.shout` is registered as
`plugin.io.example.shout.shout`. That name can't clash with another package's, and lint refuses a
package that would take a name NodeCraft itself uses. Unsigned packages can't use the reserved
`nodecraft.*` and `io.github.cyphernaught0x.*` namespaces.

## Building and checking

```sh
nodecraft-cli plugin pack examples/plugins/shout --out shout.ncraft-plugin
nodecraft-cli plugin inventory examples/plugins/shout     # the inventory and digest, no file
nodecraft-cli plugin lint shout.ncraft-plugin [--trust trust.json]
```

Add `--json` for machine-readable output.

- **`pack`** gives the same bytes, and so the same `blake3:` digest, for the same files on any
  machine and any OS. The digest is what people see when they install, and what projects record,
  so publish it with the package. `pack` refuses to write the package inside the folder it reads.
- **`lint`** opens the package with the same bounded verifier install uses (32 MiB archive,
  128 MiB expanded, 2050 entries). It then checks what a later step would refuse:
  - negotiation;
  - the namespace;
  - registered IDs against NodeCraft's own;
  - provider templates against the [network rules](plugin-wire.md#provider-operation-templates-174);
  - text action definitions, which must compile.

  It prints the rights the package will ask for. Its warnings name what installs but won't do
  anything yet: an unsigned package, an unknown signing key, a data registration this NodeCraft
  has no binding for, and dependencies to install first. Lint keeps nothing and runs nothing.

Lint in CI uses the same command. Freeze the digest of each package you publish in a test, so a
change you didn't mean to make shows up in review. Files are packed byte for byte, so a checkout
that rewrites line endings gives a different digest: mark package folders `-text` in
`.gitattributes` (`examples/plugins/** -text`).

`lint` also refuses a package that needs a right this NodeCraft can't grant (today only provider
packages can be granted `network` and `secret`), and warns about an optional one it can't.

`crates/plugin/fixtures/lint/invalid/` holds packages an author might write that pack or lint
refuses, one folder per mistake, named after the error code it gets. Use them to see what each
refusal looks like.

## Signing

An unsigned package installs as a local package. To publish one under your own name, make a key
once and sign each package with it:

```sh
nodecraft-cli plugin key new ~/keys/author.key --id my-key        # never inside a package folder
nodecraft-cli plugin key trust ~/keys/author.key --provenance "Ada's releases, https://example.org"
nodecraft-cli plugin pack examples/plugins/shout --out shout.ncraft-plugin --sign ~/keys/author.key
```

- **`key new`** writes an Ed25519 key file, readable only by you on macOS and Linux (on Windows it takes its
  folder's permissions, so keep it in a folder only you can read), and never replaces an existing
  one. Keep it out of the repository and back it up: a lost key can't sign updates.
- **`key trust`** prints the entry people add to their `trust.json` to trust your packages. Publish
  it, with its provenance note, where people can check it is yours.
- **`pack --sign`** signs the package digest. When the manifest names a `publisher_key_id`, only
  that key may sign it. `pack` refuses a key file inside the folder it packs.

Lint with `--trust` and a trust file holding your entry to see the package as signed. Error
messages name a broken field of the key file, never its contents.

## Installing and trying it

In the app, open **Plugins**, name or **Choose…** the package file, and press **Check**. The panel
shows the package's digest, its provenance and the rights it asks for. Nothing is installed until
you press **Install**. From an agent or the CLI's MCP
server, call `plugins.import` with `{"location": "/absolute/path.ncraft-plugin", "dry_run": true}`
to see the same report, and without `dry_run` to install. Then call `plugins.grant` if the package
runs anything, and `plugins.enable`.

### Trying a change: Reload

NodeCraft remembers the file each package was last installed or updated from (by absolute path, in
`sources.json` beside the store's index). To try a change, pack again to the same path and press
**Reload** beside the package in the panel, or call `plugins.reload` with `{"package_id": "…"}`. It
installs that file again as an update, after the same checks:

- **Refused, with nothing changed,** when no file is remembered (a package installed some other
  way: update it from its file once), when the file is missing or broken, when it now holds
  another package, or when it is unchanged (the same digest).
- **An enabled package** is disabled for the update, which refuses while enabled packages depend
  on it. A data package is enabled again. A package that runs anything stays disabled: grants
  cover one digest and are never carried over, so grant the new version, then enable it. The
  result says which: `enabled`, `needs_grant`, and `enable_error` if enabling again failed.
- **Nothing is watched.** Each reload is one request from you; NodeCraft never reads the file on
  its own and opens no dialog. Agents can't call it (it installs code, like `plugins.update`).

`plugins.update` (disable the package first) still works for a file anywhere else; every version is
a new, explicit install, kept for rollback.

In the browser, a package is picked in the panel and lasts until the page closes. Only `data`
packages can be enabled there, and there is no Reload: the picked file is let go once installed,
so pick it again and update.

## Compatibility: majors and minors

`host_protocol` declares the protocol major the package speaks and the range of minors it works
with. The major must equal the host's, and the host uses the highest minor both sides include.
`descriptor_contract` is versioned the same way, separately: the major must match, and the
package's minor can be no newer than the host's. This NodeCraft speaks protocol 1.0 and descriptor
contract 1.0.

- **A new minor** adds things a package may use. A package that declares `max_minor: 2` still
  installs on a 1.0 host, and works with only what 1.0 offers.
- **A new major** breaks the contract. A package for major 2 is refused by a major 1 host with
  `incompatible`, and so is a major 1 package by a major 2 host. Publish one package per major you
  support.
- **Features** are named in `features.required` and `features.optional`. A required feature the
  host doesn't know refuses the package (`unsupported_feature`). An optional one it doesn't know
  is dropped, so the package must still work without it.
- **Capabilities** (`capabilities.required` / `optional`) work the same way for host services.

`version` is your release number. It says what you intend, but NodeCraft identifies a package by
its digest. Changing files under the same version gives a new digest, and installing it is an
update like any other.

## Dependencies

A package may depend only on data packages: enabling refuses a package whose dependency runs code
(`dependency_rejected`). A dependency is locked to an exact `version` and `digest`, there is no version
resolver, and nothing is fetched. Lint warns about each dependency. The dependent package can't be
enabled until each dependency is installed, enabled and at the locked version.

## Updates, rollback and migration

- **Update** installs the new version beside the old one and swaps them in one step. The package
  must be disabled first. If the update fails (full disk, corrupt file, failed check), the old
  version stays exactly as it was.
- **Grants** cover one digest, so every update, even one that asks for no new rights, asks for
  consent again, and the package stays disabled until it is granted.
- **Rollback** (`plugins.rollback` with a kept digest, or **Roll back to …** in the panel) brings back an
  earlier version still kept in the store. It never brings back a grant that was revoked.
- **Projects**: a card made from a plugin records the package ID, version and digest it was made
  with. Opened without the package, the card stays, says what it needs, and can't run; the store
  keeps any version a project still uses when you uninstall or prune. After an update, a card made
  with the old version shows that it can be migrated. `node.migrate` (or the card's button) first
  lists every input, output and setting the new version drops or resets, then applies the change
  as one undoable edit. Keep names and settings stable between versions, so that a migration
  drops nothing.
- **Keys**: a signed package names its `publisher_key_id`, and only keys in the person's
  `trust.json` are trusted. When you rotate a key, people add the new one to their trust file
  themselves; NodeCraft never trusts a key on its own.

## When something fails

Every refusal names what failed and where. The wire error codes are listed in
[plugin-wire.md](plugin-wire.md#error-codes), and the message includes the JSON path
(`/manifest.json/host_protocol/major`) or the file.

- **Lint passes but install doesn't:** compare the digest lint printed with the one the panel
  shows. A different digest means a different file. If the digests match, the person's NodeCraft
  is a different version: check `nodecraft-cli --version` against the protocol your package
  declares.
- **It installs but does nothing:** check the lint warnings. Most often a data registration has no
  host binding yet, or the package isn't enabled or granted (`plugins.list` shows `state` and
  `grant`).
- **A job fails:** the run's error is shown on the node and in the run's log. A provider package's
  requests carry only the key it names, never the person's other keys.
- **It used to work:** `plugins.list` shows the kept versions. Roll back, and compare the two
  packages' inventories with `plugin inventory`.

## Licence and provenance checklist

- [ ] `license` is an SPDX expression that covers every file in the package.
- [ ] The licence file is in the package, with a `NOTICE` where a dependency's licence requires
      one.
- [ ] Every asset is your own work or under a permissive licence, with its source and author
      recorded. Never copy code, prompts, icons or help text from another product.
- [ ] No keys, tokens, personal paths or account names in any file. Provider packages name the key
      they need, and the person's keychain supplies it.
- [ ] The published digest matches `plugin pack` on a clean checkout.
