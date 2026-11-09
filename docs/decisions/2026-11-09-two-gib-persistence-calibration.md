## 2026-11-09 · Two-GiB native persistence calibration (#1120)

The resource-conditional proof left by [the #367 close-out](2026-11-06-persistence-off-the-ui-close-out.md)
was run after the owner requested all open medium tasks, including #1120. This is a measurement
record, not a change to the format, persistence implementation or its resource limits.

### Machine and exact scope

- Debian GNU/Linux 13, x86-64, Linux 6.18.57; a standalone managed executor, not a CI job.
- The writable filesystem was overlayfs, approximately 32 GiB, with approximately 30 GiB free
  before the probe. Its physical backing device was not exposed. The cgroup allowed 16 GiB RAM
  and four CPU cores; five logical CPUs were visible. Other agents' existing tests shared CPU.
- Rust 1.97.1 (`8bab26f4f`, 2026-07-14), debug dependency artifacts, with the checkout at
  `caecbb32c8fdc9b6f9034bc4507f5c1f35f865b6`.
- The native window used the production `NodeCraftApp`, its `eframe::App::logic`, `show`, native
  save runner, startup keep-aside and recovery dialog, inside a small eframe telemetry shell on
  the actual X11 display. The shell dispatched the real engine file commands with
  `background: true`, counted frames and exposed `Session::save_space` to the sampler. It was
  **an instrumented native UI harness, not the unmodified desktop executable**; the agent sidebar
  and desktop control-channel server were not part of this run. No provider was called.
- The build linked the existing artifacts rather than rebuilding the workspace. This identifies
  the measured bits without claiming a fresh reproducible build: UI artifact SHA-256
  `9ec98a291f3098fc756bd6be09ffa938cc9cffd4a49fb750776415232f1918b3`, engine
  `46bd083ded0236f39f2ea48a38f507bfc12835c0a82e5f82722cbe74e0ba6cbd`, format
  `e6560c08086af7b5d5f8930f0051714df79941d5142aca64dcb3d269102cd485`; probe source
  `c3e3a65926fe2423681bb7e3ed65a77c58f62dfdff3d151a183ae9f24e4b359a`.

The workload was eight distinct, valid RIFF/WAVE PCM files, each 268,435,456 bytes: mono,
48 kHz, signed 16-bit samples, a 44-byte header followed by one repeating byte value per file
(1 through 8). Their combined media size was **2,147,483,648 bytes**. Eight `source.audio`
nodes held them under the schema's `asset` parameter with matching audio output ports. The v4
project archive was 2,147,489,621 bytes. Media were streamed through `AssetStore::import_reader`;
the fixture did not allocate a two-GiB resident buffer. Audio playback was not tested.

Only temporary probe-owned files were used. The sampler enforced an 18 GiB aggregate workload
cap and a 5 GiB free-space reserve. The largest observed workload tree was 12,884,933,570 bytes;
the smallest reported free-space observation in the valid runs exceeded 13 billion bytes.
Afterwards the probe's archives, journals, private workspaces and executables were removed.

### Method and measurements

The method follows [the earlier calibration](2026-11-04-persistence-inventory-and-lock-order.md):
a separate Python sampler summed every file's logical size under the isolated workload folder
at a 10 ms interval, subtracting the size before each operation, and sampled the window process's
`/proc/<pid>/status` RSS. Those samples can miss a peak between samples and do not measure
physical-device writes or filesystem allocation overhead. Small telemetry request/reply files
were included in the tree. Wall time includes command admission and status polling; startup
also includes process/window creation and a screenshot. These are observations, not performance
thresholds or p95 claims.

Save estimates were obtained from the **same running session's `Session::save_space` immediately
before dispatch**. Open used `SpaceInventory::open` with the actual archive size. Recovery used
`ArchivePlan::estimate_bytes` on the actual replayed snapshot plus all media copied into the new
workspace, through `SpaceInventory::recover`. Startup checks replay and keep separately; the
startup row shows their sum as a conservative bound for their coexisting extents. Each check
includes production's 64 KiB-per-extent slack. The two startup extents were each 2,147,555,589 B.

| Operation | Wall time | Inventory bound (B) | Sampled peak growth (B) | Peak process RSS (B) | Frames progressed |
|---|---:|---:|---:|---:|---:|
| Open in a new native window | 4.155 s | 4,295,110,394 | 4,294,979,382 | 200,855,552 | 169 |
| Save As after that open, including verification of indexed media | 11.064 s | 8,590,214,735 | 6,442,463,031 | 201,707,520 | 264 |
| Save over the existing project | 10.010 s | 4,295,175,930 | 4,294,979,382 | 201,707,520 | 226 |
| Recover the unsaved change after SIGKILL | 6.511 s | 4,295,104,424 | 4,294,973,418 | 206,753,792 | 211 |
| Startup replay and keep-aside, after cancelling the prior launch | 4.934 s | 4,295,111,178 | 4,294,980,674 | 197,554,176 | 88 |

All measured peaks were below the corresponding bound. No underprediction was found to file.
An earlier startup measurement was lower because startup reclaimed the killed process's old
private workspace while making the new copies; the table uses the later run without that offset.
Seed creation, outside the desktop measurements, took 26.937 s and peaked at 54,079,488 B RSS
and 10,737,442,329 B of files while constructing the checkpoint and first archive.

### Cancellation, recovery and painting

The actual window remained visible, its modal progress and spinners painted, and its frame counter
advanced during every complete operation. Screenshots cover the loaded cards, startup progress,
recovery offer and recovered canvas. A frame count is evidence that work proceeded while the
window drew; this run does not establish a maximum frame latency.

- Save, open and recovery cancellation returned their named unchanged-project/file outcomes in
  0.250 s, 0.273 s and 0.306 s respectively, measured from operation admission through settlement
  (including the approximately 0.15 s delay before asking to cancel). Their frame counters
  advanced by 9, 8 and 12. Each operation was observed running before cancellation.
- A separate Save As cancellation took 0.396 s with seven frames progressing. SHA-256 of the
  destination before and after was identical:
  `4880da8a950ae9c43b02136c6a10cb3ef550a781814c1af2931870e5d565edd4`.
- After a saved project received an unsaved node-title change, SIGKILL terminated the window.
  The next launch replayed and kept the multi-GiB journal, offered the unsaved work, and its
  real **Recover** button restored it through the background recovery runner. Cancellation of
  recovery left the offer available; selecting Recover again succeeded.
- For controlled startup cancellation, the journal from the second forced quit was restored
  byte for byte from its kept copy, and its extra kept copies were removed, all within the
  isolated fixture. In the subsequent native startup, the real **Decide later** button stopped
  the operation during its reading phase. The window continued without project storage, and
  the journal's SHA-256 remained
  `3289abf3b33b80730f422079a68249c3e01e9112ec5dbd5e54c2040052f936fe`
  before and after. No partial or kept copy remained. Sixty-three frames progressed during
  that launch; its 3.717 s wall time includes window startup, screenshot capture and pointer
  input, so it is not a measurement of cancellation latency alone.
- Launching again completed the keep-aside (the table's startup row), offered the same work,
  and recovered it. Replaying that actual kept snapshot confirmed its first node title was
  `Second unsaved marker for startup cancellation` and all eight media references remained.

Two initial startup input attempts did not activate Decide later before completion; they are
excluded from cancellation evidence. The successful attempt focused the native window before
activating the observed button. An initial fixture with the wrong source parameter was also
rejected after looking at its screenshot; all measurements above use the corrected schema.

The local evidence bundle, `log/persistence-1120-2026-11-09.tar.gz`, contains the probe sources,
exact direct-rustc commands, samplers, JSON phase/status samples, provenance digests and safe
screenshots. It is a local ignored artifact, not a committed fixture or a published attachment.
No multi-GiB fixture is added to CI. This proof covers one Linux executor and an instrumented
native shell; it makes no claim about release builds, other operating systems, physical disks,
or the omitted desktop shell services. Issue status is intentionally left unchanged because
external issue tracking was not part of the requested local continuation.
