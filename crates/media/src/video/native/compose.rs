//! Rendering a [`ComposeSpec`] timeline in one bounded FFmpeg run.
use super::*;
use crate::video::timeline;

/// What a composition needs beyond the capabilities checked when the service opened.
const NEEDS: &[(&str, &str)] = &[
    ("filter=xfade", "offset"),
    ("filter=acrossfade", "curve1"),
    ("filter=tpad", "stop_mode"),
    ("filter=anullsrc", "channel_layout"),
    ("encoder=libopus", "Encoder libopus"),
];
/// What layers need on top: stacking pictures, delaying and mixing sound.
const LAYER_NEEDS: &[(&str, &str)] = &[("filter=overlay", "eof_action"), ("filter=adelay", "all"), ("filter=amix", "normalize")];
/// Charged to scratch disk as well as the composition's resident graph reservation.
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const SCRIPT_OPTIONS: [&str; 2] = ["-/filter_complex", "-filter_complex_script"];

impl VideoService {
    /// Stitch the clips of `spec` into one WebM: VP9 picture, Opus stereo sound.
    /// Shares the one render slot with [`VideoService::render`].
    pub fn compose(&self, spec: ComposeSpec) -> Result<VideoJob<RenderedVideo>> {
        for clip in spec.inputs() {
            self.check_pin(clip.source.pin())?;
        }
        for (caption, _) in spec.shown_captions() {
            self.check_pin(&caption.still.pin)?;
        }
        // Generous for a slow machine; an hour of output may take a few hours to encode.
        let budget = 120u64.saturating_add(spec.duration_us().div_ceil(1_000_000).saturating_mul(6));
        let deadline = deadline_after(Duration::from_secs(budget))?;
        if self.control.render_active.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(VideoError::Busy);
        }
        let lease = match self.control.budget.reserve(Usage { bytes: 1024 * 1024, ..Usage::default() }) {
            Ok(lease) => lease,
            Err(error) => {
                self.control.render_active.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let admission = RenderAdmission { control: Arc::clone(&self.control), writer_ready: AtomicBool::new(false), _lease: lease };
        self.submit(move |ctx, cancel| {
            let _admission = admission;
            ctx.compose(&spec, cancel, deadline)
        })
    }
}

impl Context {
    fn compose(&mut self, spec: &ComposeSpec, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<RenderedVideo> {
        let mut needs: Vec<(String, String)> = NEEDS.iter().map(|(topic, required)| ((*topic).to_owned(), (*required).to_owned())).collect();
        let captions = spec.shown_captions();
        if !spec.layers.is_empty() {
            needs.extend(LAYER_NEEDS.iter().map(|(topic, required)| ((*topic).to_owned(), (*required).to_owned())));
        }
        let mut need = |topic: String, required: String| {
            if !needs.iter().any(|(t, _)| *t == topic) {
                needs.push((topic, required));
            }
        };
        for clip in spec.inputs() {
            match &clip.source {
                Source::Video(asset) => {
                    if let Some((_, codec)) = asset.metadata.audio
                        && !clip.span.muted
                    {
                        need(format!("decoder={codec}"), format!("Decoder {codec}"));
                    }
                }
                Source::Audio(asset) => {
                    need(format!("demuxer={}", asset.demuxer), format!("Demuxer {}", asset.demuxer));
                    if !clip.span.muted {
                        let codec = asset.metadata.codec;
                        need(format!("decoder={codec}"), format!("Decoder {codec}"));
                    }
                }
                Source::Still(_) => need("demuxer=rawvideo".to_owned(), "pixel_format".to_owned()),
            }
        }
        let spans = || spec.inputs().map(|c| c.span);
        if spans().any(|s| s.placement != timeline::Placement::IDENTITY) {
            need("filter=overlay".to_owned(), "eof_action".to_owned());
            need("filter=color".to_owned(), "Filter color".to_owned());
        }
        if spans().any(|s| s.placement.rotation != 0) {
            need("filter=rotate".to_owned(), "fillcolor".to_owned());
        }
        if spans().any(|s| s.placement.opacity < 1000) {
            need("filter=lutyuv".to_owned(), "Filter lutyuv".to_owned());
        }
        if spans().any(|s| s.speed != 1000) {
            need("filter=atempo".to_owned(), "tempo".to_owned());
        }
        if !captions.is_empty() {
            need("filter=overlay".to_owned(), "eof_action".to_owned());
            need("demuxer=rawvideo".to_owned(), "pixel_format".to_owned());
        }
        let tool = self.config.tools.ffmpeg.clone();
        for (topic, required) in &needs {
            let reply = self.run(&tool, &process::args(&["-hide_banner", "-h", topic]), None, 65536, cancel, deadline)?;
            if !reply.windows(required.len()).any(|b| b == required.as_bytes()) {
                return Err(VideoError::Unsupported);
            }
        }
        let graph = timeline::graph(spec.frame, &spec.parts(), &spec.layer_parts(), &spec.caption_parts())?;
        // A pin keeps the script alive until the child is reaped. All exits, including a write,
        // probe, spawn or cancellation error, leave its file to the normal retrying cleanup.
        let (script, _) = self.compose_script(&graph)?;
        let script_name = self.compose_name(&script)?;
        let option = self.compose_script_option(&script_name, cancel, deadline)?;
        let quota = self.config.limits.output;
        let (pin, path) = self.record(quota.checked_add(64 * 1024 * 1024).ok_or(VideoError::Limit)?)?;
        let args = self.compose_args(spec, option, &script, &pin)?;
        self.encode(&args, &path, quota, None, cancel, deadline)?;
        drop(script);
        check_deadline(deadline)?;
        let size = fs::metadata(&path).map_err(|_| VideoError::Io)?.len();
        if size == 0 || size > quota {
            return Err(VideoError::Limit);
        }
        let metadata = self.probe(&path, false, ColorPolicy::RequireDeclared, true, cancel, deadline)?;
        let fps = spec.frame.fps;
        let period = u64::from(fps.1) * 1_000_000 / u64::from(fps.0);
        let expected = u128::from(spec.duration_us()) * u128::from(fps.0) / (u128::from(fps.1) * 1_000_000);
        let frames = metadata.frames.ok_or(VideoError::Process)?;
        if metadata.width != spec.frame.width
            || metadata.height != spec.frame.height
            || !metadata.frame_rate.is_some_and(|(n, d)| u128::from(n) * u128::from(fps.1) == u128::from(d) * u128::from(fps.0))
            || u128::from(frames).abs_diff(expected) > 2
            || metadata.duration_us.abs_diff(spec.duration_us()) > 2 * period + metadata.timebase.0 * 1_000_000 / metadata.timebase.1 + 1
            || metadata.audio.is_none()
        {
            return Err(VideoError::Process);
        }
        let frames = u32::try_from(frames).map_err(|_| VideoError::Limit)?;
        let asset = self.seal(pin, &path, metadata, size, cancel, deadline)?;
        Ok(RenderedVideo { asset, frames, bytes: size })
    }

    fn compose_script(&mut self, graph: &str) -> Result<(Arc<Pin>, PathBuf)> {
        if graph.len() > MAX_SCRIPT_BYTES {
            return Err(VideoError::Limit);
        }
        let (pin, path) = self.record(u64::try_from(graph.len()).map_err(|_| VideoError::Limit)?)?;
        // record has already created this file in the private service directory.
        let mut file = OpenOptions::new().write(true).truncate(true).open(&path).map_err(|_| VideoError::Io)?;
        file.write_all(graph.as_bytes()).map_err(|_| VideoError::Io)?;
        Ok((pin, path))
    }

    fn compose_script_option(&mut self, name: &std::ffi::OsStr, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<&'static str> {
        if let Some(option) = self.filter_script_option {
            return Ok(option);
        }
        let tool = self.config.tools.ffmpeg.clone();
        let option = script_option(|option| {
            // Loading options happens before -version exits, without opening codecs or inputs.
            // Probe the parser itself: vendor/nightly version strings need no interpretation.
            let mut args = process::args(&["-hide_banner", option]);
            args.push(name.to_owned());
            args.push("-version".into());
            self.run(&tool, &args, None, 65536, cancel, deadline).map(|_| ())
        })?;
        self.filter_script_option = Some(option);
        Ok(option)
    }

    /// Processes already run in the private service directory. Passing only its generated
    /// basenames prevents a long scratch parent from being repeated for up to 96 inputs.
    fn compose_name(&self, pin: &Pin) -> Result<std::ffi::OsString> {
        self.pin_path(pin)?.file_name().map(std::ffi::OsStr::to_owned).ok_or(VideoError::Invalid)
    }

    fn compose_args(&self, spec: &ComposeSpec, option: &str, script: &Pin, output: &Pin) -> Result<Vec<std::ffi::OsString>> {
        let mut args = process::args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "2",
            "-filter_complex_threads",
            "2",
        ]);
        let rate = format!("{}/{}", spec.frame.fps.0, spec.frame.fps.1);
        for clip in spec.inputs() {
            // The media read, which lasts longer or shorter in the output at the clip's speed.
            let length = timeline::seconds(clip.media_us());
            match &clip.source {
                Source::Video(asset) => {
                    args.extend(process::args(&["-noautorotate"]));
                    args.extend(probe::input_args(asset.mov));
                    args.extend(process::args(&["-ss", &timeline::seek(asset.metadata(), &clip.span), "-t", &length, "-i"]));
                }
                Source::Audio(asset) => {
                    args.extend(probe::audio_input_args(asset.demuxer));
                    args.extend(process::args(&["-ss", &timeline::seek_audio(asset.metadata(), &clip.span), "-t", &length, "-i"]));
                }
                // One raw frame: nothing to probe, and the graph holds it for the clip's length.
                Source::Still(still) => args.extend(still_input(still, &rate)),
            }
            args.push(self.compose_name(clip.source.pin())?);
        }
        // Each caption's picture, one raw frame the graph holds while the caption shows.
        for (caption, _) in spec.shown_captions() {
            args.extend(still_input(&caption.still, &rate));
            args.push(self.compose_name(&caption.still.pin)?);
        }
        args.push(option.into());
        args.push(self.compose_name(script)?);
        args.extend(process::args(&["-map", "[vout]", "-map", "[aout]", "-sn", "-dn"]));
        args.extend(process::args(&encoding(spec.quality)));
        args.extend(process::args(&["-fps_mode", "passthrough", "-c:a", "libopus", "-b:a", "128000", "-ar", "48000", "-ac", "2"]));
        args.extend(process::args(TAGS));
        args.extend(process::args(&["-fs", &self.config.limits.output.to_string()]));
        args.push(self.compose_name(output)?);
        Ok(args)
    }
}

fn script_option(mut probe: impl FnMut(&'static str) -> Result<()>) -> Result<&'static str> {
    for option in SCRIPT_OPTIONS {
        match probe(option) {
            Ok(()) => return Ok(option),
            Err(VideoError::Process) => {
                // The script was just written and pinned. Other process failures are deliberately
                // indistinguishable from unsupported syntax in this small compatibility probe.
            }
            Err(error) => return Err(error),
        }
    }
    Err(VideoError::Unsupported)
}

/// The VP9 encoding every output uses, at `quality`.
pub(super) fn encoding(quality: timeline::Quality) -> Vec<&'static str> {
    let (crf, bits) = quality.vp9();
    let mut name = "";
    VP9.iter()
        .map(|arg| {
            let value = match name {
                "-crf" => crf,
                "-b:v" => bits,
                _ => *arg,
            };
            name = arg;
            value
        })
        .collect()
}

/// The arguments before `-i` that read a staged still: one raw RGBA frame at the output's rate.
fn still_input(still: &StillAsset, rate: &str) -> Vec<std::ffi::OsString> {
    process::args(&[
        "-protocol_whitelist",
        "file",
        "-f",
        "rawvideo",
        "-pixel_format",
        "rgba",
        "-video_size",
        &format!("{}x{}", still.width, still.height),
        "-framerate",
        rate,
        "-i",
    ])
}

#[cfg(test)]
mod tests;
