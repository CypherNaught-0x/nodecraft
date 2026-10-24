## 2026-10-21 · Sound cards show a waveform and play through cpal (#106)

- **Playback is view state**, as for video (#105): play, pause and the position live in the shell
  per node; they are not commands and never touch the document or undo.
- **ffmpeg decodes, cpal plays.** The video service gains `waveform` (peaks, cached per content
  hash) and `pcm` (at most 4 s of 48 kHz stereo i16 per call); a card asks for 2 s at a time and
  keeps under 8 s buffered, so a long sound never sits in memory whole. A mono source is panned
  to both sides at full level rather than FFmpeg's quieter default upmix.
- **cpal is pinned exactly** (`=0.18.2`) and used only in `ui-egui`: it is the platform sound
  layer (CoreAudio, WASAPI, ALSA), so an upgrade is reviewed, not picked up. Its callback only
  locks the bounded buffer, resamples linearly to the device rate and plays silence on underrun.
- **One card plays at a time.** Playing another card pauses the first where it was; deleting the
  card, a lost device and quitting stop playback.
- **Linux links libasound.** Builds need `libasound2-dev`; deb/rpm packages depend on the
  library, the AppImage uses the host's, and the Flatpak takes it from its runtime and reaches the
  sound server through `--socket=pulseaudio`.
- **The browser build says playback needs the desktop app**; Web Audio is separate work.
