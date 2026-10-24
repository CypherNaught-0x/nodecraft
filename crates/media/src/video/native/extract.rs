//! Reading a video for its scene changes, and writing its sound out on its own.
use super::*;

/// The most scene changes one video reports; past it the threshold is too low to be useful.
pub const MAX_SCENES: usize = 1024;
/// The lowest and highest scene-change thresholds, in thousandths.
pub const SCENE_THRESHOLDS: std::ops::RangeInclusive<u16> = 50..=1000;
/// The scene detector's report: one short line per changed frame, well under this.
const SCENE_REPORT: usize = 256 * 1024;

impl VideoService {
    /// The times, in microseconds from the start of `asset`, where its picture changes by more
    /// than `threshold` thousandths of the scene detector's score: sorted, distinct, inside the
    /// video and after its first frame. At most [`MAX_SCENES`]; more is a limit error.
    pub fn scenes(&self, asset: &VideoAsset, threshold: u16) -> Result<VideoJob<Vec<u64>>> {
        self.check(asset)?;
        if !SCENE_THRESHOLDS.contains(&threshold) {
            return Err(VideoError::Invalid);
        }
        // Decoding is the cost: generous for a slow machine.
        let budget = 120u64.saturating_add(asset.metadata.duration_us.div_ceil(1_000_000).saturating_mul(2));
        let deadline = deadline_after(Duration::from_secs(budget))?;
        let asset = asset.clone();
        self.submit(move |ctx, cancel| ctx.scenes(&asset, threshold, cancel, deadline))
    }
    /// The first sound of `asset` as an Ogg file: copied when it is already Opus or Vorbis,
    /// otherwise encoded to Opus stereo at 48 kHz. The video itself is left as it is.
    pub fn extract_audio(&self, asset: &VideoAsset) -> Result<VideoJob<RenderedAudio>> {
        self.check(asset)?;
        if asset.metadata.audio.is_none() {
            return Err(VideoError::Invalid);
        }
        let budget = 60u64.saturating_add(asset.metadata.duration_us.div_ceil(1_000_000));
        let deadline = deadline_after(Duration::from_secs(budget))?;
        let asset = asset.clone();
        self.submit(move |ctx, cancel| ctx.extract_audio(&asset, cancel, deadline))
    }
}

impl Context {
    fn needs(&mut self, needs: &[(String, String)], cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<()> {
        let tool = self.config.tools.ffmpeg.clone();
        for (topic, required) in needs {
            let reply = self.run(&tool, &process::args(&["-hide_banner", "-h", topic]), None, 65536, cancel, deadline)?;
            if !reply.windows(required.len()).any(|b| b == required.as_bytes()) {
                return Err(VideoError::Unsupported);
            }
        }
        Ok(())
    }
    fn scenes(&mut self, asset: &VideoAsset, threshold: u16, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<Vec<u64>> {
        let needs = [("filter=select", "Filter select"), ("filter=metadata", "Filter metadata")];
        self.needs(&needs.map(|(t, r)| (t.to_owned(), r.to_owned())), cancel, deadline)?;
        let m = &asset.metadata;
        // With -copyts -start_at_zero a frame's time is its time in the asset plus this offset.
        let offset = m.stream_start.checked_sub(m.format_start).ok_or(VideoError::InvalidMetadata)?;
        // Scoring a small copy finds the same cuts and decodes no faster, but filters far less.
        let filter = format!(
            "scale=w=256:h=256:force_original_aspect_ratio=decrease:flags=bilinear,select='gt(scene\\,{}.{:03})',metadata=print:key=lavfi.scene_score:file=-",
            threshold / 1000,
            threshold % 1000
        );
        let mut args = process::args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-copyts",
            "-start_at_zero",
            "-noautorotate",
        ]);
        args.extend(probe::input_args(asset.mov));
        args.push("-i".into());
        args.push(self.path(asset)?.into_os_string());
        args.extend(process::args(&["-map", &format!("0:{}", m.stream), "-an", "-sn", "-dn", "-vf", &filter, "-f", "null", "-"]));
        let tool = self.config.tools.ffmpeg.clone();
        let report = self.run(&tool, &args, None, SCENE_REPORT, cancel, deadline)?;
        check_deadline(deadline)?;
        parse_scenes(&report, offset, m.duration_us, MAX_SCENES)
    }
    fn extract_audio(&mut self, asset: &VideoAsset, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<RenderedAudio> {
        let (stream, codec) = asset.metadata.audio.ok_or(VideoError::Invalid)?;
        let copy = matches!(codec, "opus" | "vorbis");
        let mut needs = vec![("muxer=ogg".to_owned(), "Muxer ogg".to_owned()), ("demuxer=ogg".to_owned(), "Demuxer ogg".to_owned())];
        if !copy {
            needs.push((format!("decoder={codec}"), format!("Decoder {codec}")));
            needs.push(("encoder=libopus".to_owned(), "Encoder libopus".to_owned()));
        }
        self.needs(&needs, cancel, deadline)?;
        let quota = self.config.limits.output;
        let mut args = process::args(&["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-max_alloc", "134217728"]);
        args.extend(probe::input_args(asset.mov));
        args.push("-i".into());
        args.push(self.path(asset)?.into_os_string());
        args.extend(process::args(&["-map", &format!("0:{stream}"), "-vn", "-sn", "-dn", "-map_metadata", "-1", "-map_chapters", "-1"]));
        if copy {
            args.extend(process::args(&["-c:a", "copy"]));
        } else {
            args.extend(process::args(&["-c:a", "libopus", "-b:a", "128000", "-ar", "48000", "-ac", "2"]));
        }
        args.extend(process::args(&["-fflags", "+bitexact", "-f", "ogg", "-fs", &quota.to_string()]));
        let (pin, path) = self.record(quota.checked_add(1024 * 1024).ok_or(VideoError::Limit)?)?;
        args.push(path.as_os_str().to_owned());
        self.encode(&args, &path, quota, None, cancel, deadline)?;
        check_deadline(deadline)?;
        let size = fs::metadata(&path).map_err(|_| VideoError::Io)?.len();
        if size == 0 || size > quota {
            return Err(VideoError::Limit);
        }
        let metadata = self.probe_audio(&path, "ogg", cancel, deadline)?;
        if !matches!(metadata.codec, "opus" | "vorbis") {
            return Err(VideoError::Process);
        }
        let hash = self.seal_file(&pin, &path, size, cancel, deadline)?;
        Ok(RenderedAudio { asset: AudioAsset { pin, metadata: Arc::new(metadata), hash, demuxer: "ogg" }, bytes: size })
    }
}

/// The scene times in the detector's `report`: each frame's `pts_time:` less `offset`, kept when
/// it falls after the first frame and before `duration`, sorted and distinct. Every number is
/// checked; a malformed one is refused, and more than `max` scenes is a limit error.
pub(super) fn parse_scenes(report: &[u8], offset: i128, duration: u64, max: usize) -> Result<Vec<u64>> {
    if report.len() > SCENE_REPORT {
        return Err(VideoError::Limit);
    }
    let text = std::str::from_utf8(report).map_err(|_| VideoError::InvalidMetadata)?;
    let mut times = Vec::new();
    for line in text.lines() {
        for word in line.split_ascii_whitespace() {
            let Some(value) = word.strip_prefix("pts_time:") else { continue };
            let time = probe::micros(value)?.checked_sub(offset).ok_or(VideoError::InvalidMetadata)?;
            let Ok(time) = u64::try_from(time) else { continue };
            if time == 0 || time >= duration {
                continue;
            }
            if times.len() >= max {
                return Err(VideoError::Limit);
            }
            times.push(time);
        }
    }
    times.sort_unstable();
    times.dedup();
    Ok(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_are_read_from_the_report() {
        let report = b"frame:0    pts:60      pts_time:2\nlavfi.scene_score=0.400000\nframe:1    pts:122     pts_time:4.066667\nlavfi.scene_score=0.9\n";
        assert_eq!(parse_scenes(report, 0, 10_000_000, 8).unwrap(), vec![2_000_000, 4_066_667]);
        // The stream's start offset comes off every time.
        assert_eq!(parse_scenes(report, 500_000, 10_000_000, 8).unwrap(), vec![1_500_000, 3_566_667]);
    }

    #[test]
    fn scenes_outside_the_video_or_repeated_are_dropped_and_the_rest_sorted() {
        let report = b"pts_time:5\npts_time:0\npts_time:1\npts_time:1\npts_time:10\npts_time:99\n";
        assert_eq!(parse_scenes(report, 0, 10_000_000, 8).unwrap(), vec![1_000_000, 5_000_000]);
        // Before the offset, the time is before the video.
        assert_eq!(parse_scenes(b"pts_time:0.1", 200_000, 10_000_000, 8).unwrap(), Vec::<u64>::new());
        assert_eq!(parse_scenes(b"", 0, 10_000_000, 8).unwrap(), Vec::<u64>::new());
    }

    #[test]
    fn hostile_reports_are_refused_not_trusted() {
        for bad in [
            &b"pts_time:"[..],
            b"pts_time:nan",
            b"pts_time:inf",
            b"pts_time:1e9",
            b"pts_time:-",
            b"pts_time:1.2.3",
            b"pts_time:99999999999999999999999999999999999999999",
            b"pts_time:0x10",
            b"pts_time:\xff",
        ] {
            assert!(parse_scenes(bad, 0, 10_000_000, 8).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        // A negative time is before the video, not an error.
        assert_eq!(parse_scenes(b"pts_time:-1.5", 0, 10_000_000, 8).unwrap(), Vec::<u64>::new());
        // An offset that would overflow is refused.
        assert!(parse_scenes(b"pts_time:1", i128::MIN, 10_000_000, 8).is_err());
        let many = "pts_time:1\n".repeat(9);
        assert_eq!(parse_scenes(many.as_bytes(), 0, 10_000_000, 8), Err(VideoError::Limit));
        assert_eq!(parse_scenes(&vec![b' '; SCENE_REPORT + 1], 0, 10_000_000, 8), Err(VideoError::Limit));
    }
}
