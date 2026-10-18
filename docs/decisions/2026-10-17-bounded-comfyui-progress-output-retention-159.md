# 2026-10-17 · Bounded ComfyUI progress/output retention (#159)

- `PromptProgress::new` and `apply`, and the public `files_in` extractor, now return `Result`.
  The constructor accepts borrowed string-like identifiers (`AsRef<str>`) and validates before
  copying. A failed relevant event preserves the admitted progress/output prefix but latches
  allocation-free terminal `Outcome::MetadataLimit`; the prefix is explicitly incomplete and
  subsequent success cannot overwrite the failure. Oversized terminal error metadata takes the
  same safe failure path. Unrelated prompts and events after completion remain ignored.
- A tracker retains at most 10,000 workflow IDs and 10,000 distinct `(node, FileRef)` output
  associations under an 8 MiB conservative metadata storage budget. Charges include duplicated
  strings, completed-ID reservations and spare Vec/hash-container capacity (256 bytes plus twice
  ID text per workflow node, 1024 bytes plus twice all identifier/file text per association).
  It retains only one current expanded node; only workflow IDs enter the completed set or
  contribute step progress. Exact repeated output associations are idempotent; different nodes,
  filenames, subfolders or kinds remain distinct. These are interacting caps, not a promise
  that 10,000 associations always fit: the byte budget admits fewer than 8,192 even with empty
  strings, and typically about 7,700 with file names, less workflow/current metadata. History's
  256-byte charge can admit all 10,000 files because it retains neither per-file node copies nor
  the live association dedup map; the live tracker's larger accounting is intentional.
- Constructors/events and shallow file extraction examine at most 100,000 entries. Extraction
  caps files at 10,000 and metadata at 8 MiB, charging 256 bytes per file plus all file strings.
  History shares one budget across output nodes/files and retained status/error metadata;
  `HistoryEntry::parse` keeps its signature and can now return a metadata-limit error.
  Cached-ID parser extraction preflights entry count and Vec/String storage before copying
  event fields. `parse_text` propagates extraction errors and moves the parsed raw output rather than cloning
  it to enumerate files. Valid protocol wire data and old-server terminal messages are unchanged.
- Extraction borrows only the output object's immediate arrays and file fields; unrelated nested
  JSON is never traversed or copied. Caller-owned deeply constructed Values still require safe
  caller disposal. Raw event output/preview storage is not retained by the tracker.
