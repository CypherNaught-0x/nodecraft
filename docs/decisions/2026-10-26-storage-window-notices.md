## 2026-10-26 · Preserve storage failure reasons and focus the recovery owner

## Context

A desktop window can open without persistence after failing to acquire the shared recovery
journal or start storage. File actions discarded the startup reason. A failed project Open is
different: it leaves the fresh session saveable. Explicit recovery close also disables files.

## Decision

Expose the closed/not-configured reason in engine file status and command errors. Keep a bounded,
session-scoped startup explanation in the desktop file view and reuse it in later notices. Show
the explanation in the File panel. Do not transfer or implicitly save a memory-only document.

The desktop may focus the recovery owner through `file.focus_owner`, a view-only command with
an optional host service. Reuse the existing authenticated local control transport through a
separate endpoint that admits only that command with empty parameters. Owner-only token handling
and bounded transport behavior come from the existing implementation. The client performs file
and network I/O on a worker and allows one outstanding request. General automation cannot run
this human-only command. No new OS bindings or unsafe code are introduced.

## Consequences

Two desktop processes can request focus while retaining their own documents. A window manager
may decline focus; the notice also explains manual switching. Owners from an older build or an
unreachable owner produce guidance, not a crash. An unreadable project still opens a saveable
fresh document. Closing recovery requires closing and reopening the window; no hidden journal
reacquisition can overwrite work held elsewhere.
