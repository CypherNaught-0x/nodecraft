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

impl VideoService {
    /// Stitch the clips of `spec` into one WebM: VP9 picture, Opus stereo sound.
    /// Shares the one render slot with [`VideoService::render`].
    pub fn compose(&self, spec: ComposeSpec) -> Result<VideoJob<RenderedVideo>> {
        for clip in spec.inputs() {
            self.check_pin(clip.source.pin())?;
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
        let tool = self.config.tools.ffmpeg.clone();
        for (topic, required) in &needs {
            let reply = self.run(&tool, &process::args(&["-hide_banner", "-h", topic]), None, 65536, cancel, deadline)?;
            if !reply.windows(required.len()).any(|b| b == required.as_bytes()) {
                return Err(VideoError::Unsupported);
            }
        }
        let graph = timeline::graph(spec.frame, &spec.parts(), &spec.layer_parts())?;
        let quota = self.config.limits.output;
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
            let length = timeline::seconds(clip.length_us());
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
                Source::Still(still) => args.extend(process::args(&[
                    "-protocol_whitelist",
                    "file",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgba",
                    "-video_size",
                    &format!("{}x{}", still.width, still.height),
                    "-framerate",
                    &rate,
                    "-i",
                ])),
            }
            args.push(self.pin_path(clip.source.pin())?.into_os_string());
        }
        args.extend(process::args(&["-filter_complex", &graph, "-map", "[vout]", "-map", "[aout]", "-sn", "-dn"]));
        args.extend(process::args(VP9));
        args.extend(process::args(&["-fps_mode", "passthrough", "-c:a", "libopus", "-b:a", "128000", "-ar", "48000", "-ac", "2"]));
        args.extend(process::args(TAGS));
        args.extend(process::args(&["-fs", &quota.to_string()]));
        let (pin, path) = self.record(quota.checked_add(64 * 1024 * 1024).ok_or(VideoError::Limit)?)?;
        args.push(path.as_os_str().to_owned());
        self.encode(&args, &path, quota, None, cancel, deadline)?;
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
}
