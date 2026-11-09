## 2026-11-03 · URL import is a person's host operation under a public-https policy, and My library is an explicit, bounded, cross-project store (#96)

The library import record ([2026-11-02](2026-11-02-library-import-and-attach.md)) left URL import
and a cross-project library for an owner decision. The owner asked for the hard issues to be
implemented, so these are the conservative choices made; each is easy to loosen later and none
is a file-format commitment beyond My library's own index.

**URL import**

- **A person's operation, not a command.** "Import from URL…" in the Assets tab works like
  Import files…: a person types the address, the desktop app fetches it on its own thread, and
  the bytes go to `import::import_library`, the import a picked file takes (sniffing, size caps,
  deduplication, one undo step, one outcome line). There is no command that fetches a URL, so an
  agent, the CLI or MCP can't make NodeCraft reach an address. If one is wanted later it must be
  `HUMAN_ONLY` or behind an explicit setting.
- **The policy lives in `nodecraft_engine::url_import`; the transport is the host's.** Every hop
  is checked: `https` only, port 443, no user name or password, at most 2048 characters, and a
  host that is a name or a public address (loopback, private, link-local, carrier NAT, multicast
  and documentation ranges are refused, IPv4-mapped and 6to4 forms included, and `localhost`,
  single-label names and `.local`, `.lan`, `.internal`, `.home.arpa` names). At most 3
  redirects are followed, each target checked again. The desktop transport is
  `ReqwestHttp::public_only()` (#174's): it resolves names itself and refuses a name if **any**
  address it resolves to isn't public, then connects to those checked addresses, so a rebinding
  answer can't slip in between the check and the connection. It uses no proxy (a proxy would
  resolve names itself), keeps no cookies, follows no redirects itself and sends the generic
  `NodeCraft/<version>` User-Agent. The only request header is `Accept`.
- **Bounded.** At most 16 MiB (one small project asset), counted as the body streams in and
  aborted past it; 60 seconds per hop; Cancel stops the download at its next piece and before
  the next hop, and a cancelled or abandoned fetch is never imported. The body of a reply that
  isn't 2xx (a redirect, a refusal) is never read: the transport's
  `send_streaming_no_redirect_bodiless_errors` returns its status and headers and closes the
  connection, so a huge or slow redirect body costs nothing. Other transports and callers keep
  their behaviour (the trait's default reads and drops the body).
- **Media only, twice checked.** The reply's `Content-Type` must be an image, video, audio, 3D
  model or PDF type, or only say `application/octet-stream`; the bytes must then sniff as media
  of the same kind. A web page, JSON or text is refused even if its bytes look like media.
- **The address stays out of the project.** The card is titled with the last path segment,
  percent-decoded and cleaned as a picked file's name is (no folder, no query); with no usable
  segment it is "Download from <host>". Messages and the import report name the host only, never
  the path, query or a token in it. Nothing else of the address is stored.
- **Not in the browser.** A web page's `fetch` can't refuse private addresses or rebinding, so
  the browser build disables the button and says to download the file and drop it.

**My library (the cross-project library)**

- **Same shape as the Elements library.** `assets/library.json` (versioned) and
  `assets/media/<blake3>` in the settings folder (`FileAssetLibrary`), in memory for the browser
  and headless hosts (`MemoryAssetLibrary`). An entry is the media reference (hash, type,
  header facts), a public name and the **title** of the project it was saved from: never a path,
  canvas, card, URL or provider handle. Listings and errors name the index file
  (`library.json`), never its folder, which holds the user name.
- **Only what a person saves goes in.** Nothing is added automatically, private canvases
  included: "Save to My library" (ticked assets or results) runs `library.save_assets`, and both
  it and `library.unsave_assets` are `HUMAN_ONLY`, as `element.save` is. Agents may list
  (`library.saved`) and place (`library.place_saved`). That is deliberate and worth knowing: an
  agent working in one project can see the titles of the person's other projects that saved
  files (`origin`) and place those files into this one. Nothing else of another project is
  visible.
- **Bounded by refusal, not eviction.** At most 4096 entries and 1 GiB of media, each at most
  16 MiB. A save that doesn't fit fails for that file with the reason; nothing is ever evicted,
  because these are files a person chose to keep. #147's storage work bounds project stores,
  not this one, so the caps are My library's own.
- **Placing copies.** `library.place_saved` adds one titled source card per entry in a grid, as
  one undo step, through the `Place` effect the Elements library uses (now told which library to
  copy from): files the project lacks are read back, checked against their hash and admitted
  through `prepare_imports`. A copied file must also be recognised from its bytes as the type its
  entry names, so a hand-edited index can't put a PDF on an image card. Projects never refer back
  to My library, so removing an entry leaves every project that placed it working.
- **Corruption is reported, then repaired by a person; never a panic.** Listing never writes: an
  index that can't be parsed (or is over its caps) is listed as `index_damaged` with nothing in
  it, and saving or removing is refused until **Repair** (`library.repair_saved`, `HUMAN_ONLY`)
  runs. That keeps a read (which agents may run) from hashing and probing up to a gigabyte, and
  makes the slow step one a person chose. Repair keeps the damaged index aside under the first
  free name (`library.damaged.json`, then `library.damaged-2.json`, …, never over an earlier
  copy) and rebuilds the index from the files kept, reading at most 1 GiB: each file that still
  matches its hash and is recognised becomes "Recovered <kind>" (names and origins were in the
  index). A file that matches its hash but this build can't use, or that the budget left
  unchecked, is kept as an unusable entry, so no later tidying deletes it; only a file that no
  longer matches its hash is dropped. An entry this build can't use is kept as written and
  counted. A newer format version is reported and left alone.
- **Windows take turns; reads are as last listed.** (On Windows a replace can fail while another
  program has the file open; that is reported as a failed save to try again.) Every change takes `library.lock`, reads the
  index again, edits it and replaces it atomically, so two windows never lose each other's save.
  Listing reads the file again (the panel has Refresh); placing uses the entries as last listed,
  and an entry another window removed meanwhile is refused with "list it again". Files no entry
  holds are deleted after each change; a file not named by a hash is left alone.

**Starter templates as techniques** are not changed here. The bundled, versioned templates
from the external repository (#953) are already listed and added from the Library tab; keeping
downloaded updates in a verified cache is #954, which is open and stands on its own.
