# Original timeline interchange fixture

`moving-marker.mp4` is NodeCraft's original 32×32, one-second, twelve-frame blue/orange
orientation/motion fixture with a 440 Hz tone. It was authored in #952, and this self-contained
copy preserves the timeline export regression/sample independently of template packaging.
No external video, workflow or editor material is included. It uses the repository's license.
The exact original authoring expression is recorded in #952's fixture history.

The Rust sample author creates its own small colored PNG and stereo PCM WAV in memory. The
ignored export test emits ZIPs only to an explicitly selected evidence directory; those ZIPs
are for actual Final Cut Pro/Resolve import checks and are not evidence that either app imported
them. No generated screenshots or editor screenshots are committed.
