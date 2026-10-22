## 2026-10-22 · Timeline filter graphs live in charged scratch files (#642)

- A composition writes its generated graph into a service-owned scratch record, capped at
  1 MiB and charged to the existing disk budget. Its pin lasts until the child is reaped, and
  the existing retrying cleanup removes it on completion, error or cancellation.
- The renderer probes the option parser with the script and `-version`, without opening any
  inputs or codecs. It prefers `-/filter_complex` (FFmpeg 7 onwards), then tries the older
  `-filter_complex_script` only after a process failure. Cancellation, timeout and resource
  errors propagate immediately. The successful syntax is cached; no version string is guessed.
  See [FFmpeg's file-loading option syntax](https://ffmpeg.org/ffmpeg.html#Options).
- Composition inputs, script and output use only generated record basenames. The child already
  runs in the private service directory, so a long scratch parent is not repeated for each
  input. No project filename or filter string reaches the command line.
- A regression test bounds the complete Windows command, including conservative quoting and
  the maximum executable path, at all 32 clips/layers and 64 captions. Repeated layer additions
  obey that same cumulative clip limit. An installed-tool test renders 32 clips including eight
  picture layers and all 64 captions; it can run on Windows with explicitly configured tools.
