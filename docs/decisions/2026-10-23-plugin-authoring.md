## 2026-10-23 · Plugin packages are packed and linted by the CLI, with the install verifier (#175)

- **One verifier.** `nodecraft-cli plugin lint` installs the package into a store held in memory,
  as a dry run, with the same bounded verifier, negotiation and namespace rules as
  `plugins.import`. It then runs the checks that granting and enabling would run: registered IDs
  against NodeCraft's own, provider template rules, and compiling text actions. Lint is therefore
  never more lenient than install, and stays in step with it without a second implementation.
  Nothing is kept and nothing runs.
- **Reproducible packing.** `plugin pack` writes entries in path order, stored, with the ZIP
  writer's fixed timestamp, and writes the manifest in its canonical form. The same files give the
  same bytes and digest on every OS. `.gitattributes` keeps the examples' line endings, so their
  frozen digests hold on Windows checkouts. The packed archive is opened again with the install
  verifier before it is written, so `pack` never writes a package that install would refuse for
  its shape.
- **Folders are read conservatively.** Names starting with `.` are skipped, while links, special
  files and names that aren't UTF-8 stop the pack. The read is capped at the inventory's file count
  and the package's expanded size. The output may not land inside the folder being packed.
- **The host's taken IDs are computed in one place**
  (`nodecraft_engine::plugin_cmds::host_ids`): the built-in commands and the default generation
  catalog. The app and lint share it.
- **Not yet.** Signing tooling, a mock broker for executable runtimes and an explicit
  `plugins.reload` are follow-up issues. There is no Rust action example, because the WebAssembly
  runtime doesn't exist yet.
