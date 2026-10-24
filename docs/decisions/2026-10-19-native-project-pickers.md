## 2026-10-19 · Native project pickers are UI requests, never engine dialogs (#407)

Open, Save and Save As obtain native locations through exactly pinned rfd0.17.2 at the UI layer.
The existing engine file commands remain the only mutation path. CLI, MCP and control calls do
not construct pickers. Browser project storage and pickers remain unsupported in the web preview.

The UI constructs and polls one asynchronous picker on its main thread. AppKit discovers the
application/window during construction, so spawning arbitrary dialog threads is not equivalent.
An egui repaint waker delivers completion; there is no periodic polling timer or new async runtime.
No future borrows the app or Session. Dropping an app cannot make a later picker result dispatch.
Native picker internals are platform services; deterministic kittests use an injected boundary.

A pending request retains one Session pointer identity and monotonic revision, including its
Save-before-Open continuation. Edits, undo/redo, Open/Recover or Session replacement invalidate
it before dispatch. A stale OS picker keeps its single slot until completion; it cannot admit a
second native sheet. Dirty Open requires Save, Discard or Cancel for that exact state. Cancelled
or failed Save never proceeds to Open. Uncommitted field drafts must be finished or discarded
explicitly first; they are not silently omitted from a save or replaced with another document.
Picker/confirmation and the File panel own canvas inputs through their completion frame; held
minimap and other gestures refuse new requests. Existing perf-report clipboard routing is unchanged.

Picker paths are UTF8, absolute and at most4096 bytes without NUL, matching command admission.
Open requires an ordinary file; Save may name an absent file in an existing directory. Complete
canonical aliases retain the native storage contract, while dangling aliases and devices refuse.
The native storage layer remains authoritative for locks, size limits, atomic publication and
cooperative filesystem races. No project bytes are read in the picker layer. A valid explicit
filename/extension is preserved: Open offers NodeCraft/all-file filters, while Save suggests
`Untitled.ncraft` without a filter that could append another extension on Windows.

Recent projects are local filesystem locations in the existing additive settings schema. At most
16 unique paths of4096 bytes are retained, considering at most256 supplied entries. They are
remembered only after a successful file adoption/save, including successful publication followed
by recovery-checkpoint failure. Clean adopted locations also reflect startup/control Open or
SaveAs; this is recent-list presentation, never save/deletion authority. Missing entries remain
until explicit removal. Read-only or failed settings persistence leaves the file operation successful
and reports that recents were not saved. Comments, unknown settings and provider/account fields survive.

Current-main integration with #461: native menu/shortcut Save, selected Save As and confirmed Save-before-Open opt into background saving. The controller retains input ownership until polling confirms publication; only then are the destination remembered and the selected project opened. Failure or stale document identity/revision cancels continuation. Quit waits through the existing storage close path and never executes a delayed Open. Engine/CLI/control defaults remain synchronous. Hosts unable to start a worker retain the existing inline fallback.
