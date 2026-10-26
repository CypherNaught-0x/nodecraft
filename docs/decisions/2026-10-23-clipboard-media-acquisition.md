## 2026-10-23 · Bound clipboard and browser file acquisition (#633)

Keep text editors and serialized canvas payloads on their existing path. On desktop, arbitrate
plain canvas text off the UI thread: Finder/Explorer may also expose names as plain text, so
typed file references win on a positive receipt. If the optional file-reference probe is absent
or fails, deliver the text egui already supplied without reading pixels. Failure to start the
probe worker also falls back to that text. A confirmed file list whose bounded acquisition
fails is reported as a file error instead of silently importing its plain-text names.
A textless paste reads typed references before image pixels through the pinned safe arboard API. File references enter the existing
bounded native drop service, retaining its single worker, admission lease, path limits and
blocked-I/O policy. No clipboard path string is interpreted as a file reference.

In the browser, a trusted paste event records only a bounded intent flag. The next admitted
canvas frame calls Clipboard.read before awaiting; text editors, popups and ordinary text
paste keep their input. Catch missing APIs, synchronous exceptions and rejected promises.
Select only image/png, then use an owned FileReader. Clipboard acquisition has a ten-second
frame-observed deadline. An unsettled permission task retains one busy permit after UI timeout,
so repeated gestures cannot create replacement permission requests. Late tasks own weak state,
not a Session or a mutation callback.

Browser drops also own abortable FileReaders. Inspect the first 32 files, bound each body to
16 MiB and retained Rust byte vectors to 48 MiB before allocating or copying. Escape cancels
only when no other input owner needs it. Timeout, cancellation or document replacement drops
readers and ignores late results. Browser buffers, OS clipboard contents, codec allocation and
synchronous engine work are separate owners; this is not a total-RSS bound.

Capture private session identity, project, canvas, revision and placement at admission. Deliver
only while that target is current, through the existing engine file importer, as one undo step.
The captured placement is the payload center, and both PNG paste and copied files use the
same geometry-aware importer as current native drops. Use kittests for input ownership,
failed/late completion, bounded cleanup and copied native
files. Actual clipboard permission and gesture behavior still needs evidence from a served
wasm build; native fake clipboard ports cannot establish browser support.
