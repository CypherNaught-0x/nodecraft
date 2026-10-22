## 2026-10-22 · Discover installed video tools in the desktop host (#411)

The desktop app already compiles the native video feature through `ui-egui`'s native dependency.
It now resolves each tool once before the frame loop: an absolute environment override
(`NODECRAFT_FFMPEG` or `NODECRAFT_FFPROBE`), then its absolute `[media]` setting, then the first
executable `ffmpeg` / `ffprobe` on `PATH` (`.exe` on Windows). The two tools may come from
different sources. An invalid explicit path refuses discovery rather than falling back.
The media worker still checks existence and capabilities before admitting work.

This completes the discovery deferred by [video on cards](2026-10-21-video-on-cards.md).
Discovery skips relative and empty PATH entries, so a project folder cannot supply an
executable through the current directory. It searches at most 256 entries and 64 KiB of PATH
data. Filesystem checks happen at native startup, never while drawing a frame; an operating
system filesystem call can still stall startup. No process is spawned by discovery. Process
execution and quotas stay inside the existing bounded media service.

Resolved paths are used together by video previews, sound previews and the render host. They
are not saved into the settings file or project. Changing the environment or installed tools
requires restarting the app. Missing tools produce an install/configuration explanation on
the card; invalid overrides produce a startup notice. The browser retains its explicit desktop
requirement. No FFmpeg binary, installer change or codec licence claim is introduced.
