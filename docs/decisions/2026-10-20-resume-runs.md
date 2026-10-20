## 2026-10-20 · Runs survive a restart through a run journal (#508)

The run host keeps a **run journal** (`runs.json`): the usage log, and for every paid job the
provider accepted and that hasn't ended, its provider handle, the key and generation it was sent
under, its card and project, where its outputs go, and what its step would record. The host
saves it atomically (the old file stays whole if the write is cut short) when a job's handle
arrives or a job ends, and checks the usage log for other changes every two seconds while work
is in flight. Until now the usage log lived only in memory, so this is also what keeps spending
across launches.

**A paid job's estimate is saved before the job is sent** (write-ahead): the runner saves the
usage log, with the jobs as last saved, right before the submit. A crash after the submit then
leaves an open estimate with no handle, which counts as below; without the write-ahead it could
leave nothing, and a billed job would never be counted. If that save fails the job isn't sent
and says why. A journal that couldn't be read back (too large, too many jobs) is never written.

**After a restart a submitted job is followed, never sent again.** Its registration is
resume-only: with no handle it fails rather than submitting, since a second submit could be
billed twice. It is polled from the saved handle once its provider is set up (the desktop asks
for the provider at startup, like Generate does), and its saved estimate is adopted rather than
charged again. When it ends its charge settles like any other job's: the reported cost when it
finishes, and **unresolved** at the estimate when it fails after possibly being billed or can't
be followed at all.

**An estimate left open with no saved handle is charged as unresolved** when the journal opens.
Its submit may or may not have reached the provider, which only the provider knows; caps keep
counting it, which is the conservative outcome.

**Only a step with exactly one job and no fan-out is recorded after a restart.** Its result is
that job's alone, so the generation can be assembled from what was saved when it was queued
(input hash, settings, output ports) plus the outputs. A job of a fanned or multi-job step is
followed to its end so its charge settles, and its card says why nothing was recorded; rebuilding
a half-finished run is out of scope. A finished job is recorded only into its own project, and
only if its card still exists.

**Jobs of another project wait** in the journal until that project is open, and their estimates
stay counted meanwhile; after 30 launches without it they are given up on and settled as
unresolved. A job that finishes while its project is closed waits the same way and is followed
again (its charge already settled) to record its result. Jobs aren't followed while the startup
recovery dialog is open, since recovering would replace the document they record into. A job whose provider can't be reached after three tries is kept for the
next launch rather than settled, since it may still finish.

**Quitting suspends instead of cancelling** when a journal is attached: what is running stays
at the provider and is saved, to be followed next launch. From then on nothing new is sent, so
no job can be sent after that last save. Without a journal (tests, a window
that doesn't own storage) quitting cancels as before.

**One window owns the journal.** The desktop keeps it beside its recovery journal and opens it
only when this window acquired project storage, whose lock keeps a second window from following
and saving the same jobs. A journal that can't be read is renamed `runs.json.damaged`, not
overwritten, and the person is told; if it can't be moved aside, nothing is saved that session.
A failed save is reported once and retried every few seconds.
