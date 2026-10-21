use super::*;
use serde_json::Value;
fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
fn number(v: &Value, key: &str) -> Result<u64> {
    v.get(key).and_then(Value::as_u64).or_else(|| text(v, key).and_then(|s| s.parse().ok())).ok_or(VideoError::InvalidMetadata)
}
/// Decimal seconds to ceiling microseconds, with bounded exact integer arithmetic.
pub(super) fn micros(s: &str) -> Result<i128> {
    if s.len() > 64 || s.is_empty() {
        return Err(VideoError::InvalidMetadata);
    }
    let neg = s.starts_with('-');
    let s = if neg { s.get(1..).ok_or(VideoError::InvalidMetadata)? } else { s };
    let (w, f) = s.split_once('.').unwrap_or((s, ""));
    if w.is_empty() || !w.bytes().chain(f.bytes()).all(|b| b.is_ascii_digit()) || f.len() > 18 {
        return Err(VideoError::InvalidMetadata);
    }
    let whole = w.parse::<i128>().map_err(|_| VideoError::InvalidMetadata)?;
    let den = 10i128.checked_pow(u32::try_from(f.len()).map_err(|_| VideoError::InvalidMetadata)?).ok_or(VideoError::InvalidMetadata)?;
    let frac = if f.is_empty() { 0 } else { f.parse::<i128>().map_err(|_| VideoError::InvalidMetadata)? };
    let n = whole.checked_mul(den).and_then(|v| v.checked_add(frac)).and_then(|v| v.checked_mul(1_000_000)).ok_or(VideoError::InvalidMetadata)?;
    Ok(if neg { -(n / den) } else { n.checked_add(den - 1).ok_or(VideoError::InvalidMetadata)? / den })
}
fn ratio(s: &str, sep: char, cap: u64) -> Result<(u64, u64)> {
    if s.len() > 64 {
        return Err(VideoError::InvalidMetadata);
    }
    let (a, b) = s.split_once(sep).ok_or(VideoError::InvalidMetadata)?;
    let a = a.parse::<u64>().map_err(|_| VideoError::InvalidMetadata)?;
    let b = b.parse::<u64>().map_err(|_| VideoError::InvalidMetadata)?;
    if a == 0 || b == 0 || a > cap || b > cap {
        return Err(VideoError::InvalidMetadata);
    }
    Ok((a, b))
}
fn color(s: &Value, policy: ColorPolicy) -> Result<VideoColorInfo> {
    if ["color_space", "color_primaries", "color_transfer", "color_range"].iter().any(|k| text(s, k).is_some_and(|v| v.len() > 32)) {
        return Err(VideoError::InvalidMetadata);
    }
    let original = ["color_space", "color_primaries", "color_transfer", "color_range"]
        .map(|k| text(s, k).filter(|v| !matches!(*v, "unknown" | "unspecified")).map(str::to_owned));
    if policy == ColorPolicy::RequireDeclared && original.iter().any(Option::is_none) {
        return Err(VideoError::Unsupported);
    }
    let profile = match original.first().and_then(Option::as_deref).or_else(|| original.get(1).and_then(Option::as_deref)) {
        Some("bt709") | None => ColorProfile::Bt709,
        Some("smpte170m") => ColorProfile::Smpte170m,
        Some("bt470bg") => ColorProfile::Bt470bg,
        _ => return Err(VideoError::Unsupported),
    };
    let (p, t) = match profile {
        ColorProfile::Bt709 => ("bt709", "bt709"),
        ColorProfile::Smpte170m => ("smpte170m", "smpte170m"),
        ColorProfile::Bt470bg => ("bt470bg", "bt709"),
    };
    if original.get(1).and_then(Option::as_deref).is_some_and(|v| v != p)
        || original.get(2).and_then(Option::as_deref).is_some_and(|v| v != t && !(profile != ColorProfile::Bt709 && v == "bt709"))
    {
        return Err(VideoError::Unsupported);
    }
    let full_range = match original.get(3).and_then(Option::as_deref) {
        None | Some("tv") => false,
        Some("pc") => true,
        _ => return Err(VideoError::Unsupported),
    };
    let assumed_fields = original.iter().enumerate().fold(0, |bits, (i, v)| bits | if v.is_none() { 1u8 << i } else { 0 });
    Ok(VideoColorInfo { original, profile, full_range, assumed_fields })
}
pub(super) fn parse(bytes: &[u8], policy: ColorPolicy) -> Result<VideoMetadata> {
    if bytes.len() > 262144 {
        return Err(VideoError::Limit);
    }
    preflight_json(bytes)?;
    let v: Value = serde_json::from_slice(bytes).map_err(|_| VideoError::InvalidMetadata)?;
    let streams = v.get("streams").and_then(Value::as_array).ok_or(VideoError::InvalidMetadata)?;
    if streams.len() > 16 {
        return Err(VideoError::Limit);
    }
    let s = streams.iter().find(|s| text(s, "codec_type") == Some("video")).ok_or(VideoError::Unsupported)?;
    if !matches!(text(s, "codec_name"), Some("h264" | "vp8" | "vp9"))
        || !matches!(text(s, "pix_fmt"), Some("yuv420p" | "yuv422p" | "yuv444p"))
        || text(s, "field_order").is_some_and(|v| !matches!(v, "progressive" | "unknown"))
    {
        return Err(VideoError::Unsupported);
    }
    let width = u32::try_from(number(s, "width")?).map_err(|_| VideoError::Limit)?;
    let height = u32::try_from(number(s, "height")?).map_err(|_| VideoError::Limit)?;
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || u64::from(width) * u64::from(height) > 16_777_216
    {
        return Err(VideoError::Limit);
    }
    let tb = ratio(text(s, "time_base").ok_or(VideoError::InvalidMetadata)?, '/', 1_000_000_000)?;
    let format = v.get("format").ok_or(VideoError::InvalidMetadata)?;
    let format_start = micros(text(format, "start_time").ok_or(VideoError::Unsupported)?)?;
    let stream_start = micros(text(s, "start_time").ok_or(VideoError::Unsupported)?)?;
    if !(-3_600_000_000..=3_600_000_000).contains(&format_start) || !(-3_600_000_000..=3_600_000_000).contains(&stream_start) || stream_start < format_start {
        return Err(VideoError::Unsupported);
    }
    if let Some(pts) = s.get("start_pts").and_then(Value::as_i64) {
        let actual = i128::from(pts) * i128::from(tb.0) * 1_000_000 / i128::from(tb.1);
        let tick = i128::from(tb.0) * 1_000_000 / i128::from(tb.1) + 1;
        if actual.abs_diff(stream_start) > u128::try_from(tick).map_err(|_| VideoError::InvalidMetadata)? {
            return Err(VideoError::InvalidMetadata);
        }
    }
    let declared_duration = text(s, "duration").map(micros).transpose()?;
    let duration = if let Some(ticks) = s.get("duration_ts") {
        let ticks = ticks.as_u64().or_else(|| ticks.as_str().and_then(|value| value.parse::<u64>().ok())).ok_or(VideoError::InvalidMetadata)?;
        let numerator = u128::from(ticks).checked_mul(u128::from(tb.0)).and_then(|value| value.checked_mul(1_000_000)).ok_or(VideoError::InvalidMetadata)?;
        let exact = i128::try_from(numerator.div_ceil(u128::from(tb.1))).map_err(|_| VideoError::InvalidMetadata)?;
        let tick = i128::from(tb.0) * 1_000_000 / i128::from(tb.1) + 1;
        if declared_duration.is_some_and(|value| value.abs_diff(exact) > u128::try_from(tick).unwrap_or(0)) {
            return Err(VideoError::InvalidMetadata);
        }
        exact
    } else if let Some(duration) = declared_duration {
        duration
    } else {
        let offset = stream_start.checked_sub(format_start).ok_or(VideoError::InvalidMetadata)?;
        micros(text(format, "duration").ok_or(VideoError::Unsupported)?)?.checked_sub(offset).ok_or(VideoError::InvalidMetadata)?
    };
    if duration <= 0 || duration > 3_600_000_000 {
        return Err(VideoError::Unsupported);
    }
    let sar = ratio(text(s, "sample_aspect_ratio").unwrap_or("1:1"), ':', 65535)?;
    if s.get("tags").and_then(|v| v.get("rotate")).is_some() {
        return Err(VideoError::Unsupported);
    }
    let mut rotation = None;
    if let Some(list) = s.get("side_data_list").and_then(Value::as_array) {
        if list.len() > 16 {
            return Err(VideoError::Limit);
        }
        for data in list {
            if let Some(matrix) = text(data, "displaymatrix") {
                let mut values = Vec::new();
                for row in matrix.lines().filter(|r| !r.trim().is_empty()) {
                    let (_, numbers) = row.split_once(':').ok_or(VideoError::Unsupported)?;
                    for n in numbers.split_whitespace() {
                        if values.len() >= 9 {
                            return Err(VideoError::Unsupported);
                        }
                        values.push(n.parse::<i64>().map_err(|_| VideoError::Unsupported)?);
                    }
                }
                let clockwise = match values.as_slice() {
                    [65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824] => 0,
                    [0, -65536, 0, 65536, 0, 0, 0, 0, 1073741824] => 270,
                    [-65536, 0, 0, 0, -65536, 0, 0, 0, 1073741824] => 180,
                    [0, 65536, 0, -65536, 0, 0, 0, 0, 1073741824] => 90,
                    _ => return Err(VideoError::Unsupported),
                };
                let declared = data.get("rotation").and_then(Value::as_i64).ok_or(VideoError::Unsupported)?;
                if declared.rem_euclid(90) != 0 || (360 - declared.rem_euclid(360)).rem_euclid(360) != i64::from(clockwise) {
                    return Err(VideoError::InvalidMetadata);
                }
                if rotation.is_some_and(|previous| previous != clockwise) {
                    return Err(VideoError::InvalidMetadata);
                }
                rotation = Some(clockwise);
            } else if data.get("rotation").is_some() {
                return Err(VideoError::Unsupported);
            }
        }
    }
    Ok(VideoMetadata {
        width,
        height,
        duration_us: u64::try_from(duration).map_err(|_| VideoError::InvalidMetadata)?,
        stream: u32::try_from(number(s, "index")?).map_err(|_| VideoError::Limit)?,
        rotation: rotation.unwrap_or(0),
        sar: (u32::try_from(sar.0).map_err(|_| VideoError::Limit)?, u32::try_from(sar.1).map_err(|_| VideoError::Limit)?),
        frame_rate: text(s, "avg_frame_rate").and_then(|s| ratio(s, '/', 1_000_000_000).ok()),
        color: color(s, policy)?,
        format_start,
        stream_start,
        timebase: tb,
        frames: text(s, "nb_read_frames").and_then(|v| v.parse().ok()),
        audio: audio(streams),
    })
}
/// The first audio stream, if its codec is one of the few we decode; any other audio is ignored.
fn audio(streams: &[Value]) -> Option<(u32, &'static str)> {
    let s = streams.iter().find(|s| text(s, "codec_type") == Some("audio"))?;
    Some((u32::try_from(number(s, "index").ok()?).ok()?, decoder(text(s, "codec_name")?)?))
}
/// The FFmpeg decoder for an audio codec we decode.
fn decoder(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "aac" => "aac",
        "opus" => "opus",
        "vorbis" => "vorbis",
        "mp3" => "mp3",
        "flac" => "flac",
        "pcm_s16le" => "pcm_s16le",
        "pcm_s24le" => "pcm_s24le",
        "pcm_f32le" => "pcm_f32le",
        "pcm_u8" => "pcm_u8",
        _ => return None,
    })
}
/// The demuxer a sound file of this sniffed type is read with; any other type is refused.
pub(super) fn audio_demuxer(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/mpeg" => "mp3",
        "audio/aac" => "aac",
        "audio/mp4" | "video/mp4" | "video/quicktime" => "mov",
        "video/webm" | "video/x-matroska" => "matroska",
        _ => return None,
    })
}
/// A sound file's metadata, from ffprobe's JSON: its first audio stream, refused when that
/// stream's codec isn't one we decode.
/// Any picture (cover art, a video track) is ignored.
pub(super) fn parse_audio(bytes: &[u8]) -> Result<AudioMetadata> {
    if bytes.len() > 262144 {
        return Err(VideoError::Limit);
    }
    preflight_json(bytes)?;
    let v: Value = serde_json::from_slice(bytes).map_err(|_| VideoError::InvalidMetadata)?;
    let streams = v.get("streams").and_then(Value::as_array).ok_or(VideoError::InvalidMetadata)?;
    if streams.len() > 16 {
        return Err(VideoError::Limit);
    }
    let s = streams.iter().find(|s| text(s, "codec_type") == Some("audio")).ok_or(VideoError::Unsupported)?;
    let codec = text(s, "codec_name").and_then(decoder).ok_or(VideoError::Unsupported)?;
    let rate = number(s, "sample_rate")?;
    let channels = number(s, "channels")?;
    if !(8000..=384_000).contains(&rate) || !(1..=8).contains(&channels) {
        return Err(VideoError::Unsupported);
    }
    let format = v.get("format").ok_or(VideoError::InvalidMetadata)?;
    // Raw streams (ADTS, some WAVs) may not say where they start: they start at 0.
    let format_start = text(format, "start_time").map(micros).transpose()?.unwrap_or(0);
    let stream_start = text(s, "start_time").map(micros).transpose()?.unwrap_or(format_start);
    if !(-3_600_000_000..=3_600_000_000).contains(&format_start) || !(-3_600_000_000..=3_600_000_000).contains(&stream_start) || stream_start < format_start {
        return Err(VideoError::Unsupported);
    }
    let duration = match text(s, "duration").map(micros).transpose()? {
        Some(duration) => duration,
        None => {
            let offset = stream_start.checked_sub(format_start).ok_or(VideoError::InvalidMetadata)?;
            micros(text(format, "duration").ok_or(VideoError::Unsupported)?)?.checked_sub(offset).ok_or(VideoError::InvalidMetadata)?
        }
    };
    if duration <= 0 || duration > 3_600_000_000 {
        return Err(VideoError::Unsupported);
    }
    Ok(AudioMetadata {
        duration_us: u64::try_from(duration).map_err(|_| VideoError::InvalidMetadata)?,
        stream: u32::try_from(number(s, "index")?).map_err(|_| VideoError::Limit)?,
        codec,
        format_start,
        stream_start,
    })
}
/// The bounded input options for a sound file read with `demuxer`.
pub(super) fn audio_input_args(demuxer: &'static str) -> Vec<std::ffi::OsString> {
    let mut a = process::args(&[
        "-protocol_whitelist",
        "file,pipe",
        "-probesize",
        "8388608",
        "-analyzeduration",
        "5000000",
        "-max_probe_packets",
        "2048",
        "-threads",
        "2",
        "-f",
        demuxer,
    ]);
    if demuxer == "mov" {
        a.extend(process::args(&["-enable_drefs", "0", "-use_absolute_path", "0"]));
    }
    a
}
pub(super) fn input_args(mov: bool) -> Vec<std::ffi::OsString> {
    let mut a = process::args(&[
        "-protocol_whitelist",
        "file,pipe",
        "-probesize",
        "8388608",
        "-analyzeduration",
        "5000000",
        "-max_probe_packets",
        "2048",
        "-threads",
        "2",
        "-f",
        if mov { "mov" } else { "matroska" },
    ]);
    if mov {
        a.extend(process::args(&["-enable_drefs", "0", "-use_absolute_path", "0"]));
    }
    a
}
pub(super) fn filter(m: &VideoMetadata, w: u32, h: u32) -> String {
    let (p, t) = match m.color.profile {
        ColorProfile::Bt709 => ("bt709", "bt709"),
        ColorProfile::Smpte170m => ("smpte170m", "smpte170m"),
        ColorProfile::Bt470bg => ("bt470bg", "bt709"),
    };
    let t = m.color.original.get(2).and_then(Option::as_deref).unwrap_or(t);
    let range = if m.color.full_range { "pc" } else { "tv" };
    let rotate = match m.rotation {
        90 => "transpose=clock,",
        180 => "hflip,vflip,",
        270 => "transpose=cclock,",
        _ => "",
    };
    format!(
        "colorspace=ispace={p}:iprimaries={p}:itrc={t}:irange={range}:all=bt709:trc=srgb:range=pc:format=yuv444p:fast=0:dither=none,{rotate}scale={w}:{h}:flags=bilinear:in_color_matrix=bt709:out_color_matrix=bt709:in_range=pc:out_range=pc,setsar=1,format=rgba"
    )
}

// Bound nesting, field count and encoded strings before serde constructs an owned tree.
fn preflight_json(bytes: &[u8]) -> Result<()> {
    let mut depth = 0usize;
    let mut fields = 0usize;
    let mut tokens = 0usize;
    let mut string = false;
    let mut escape = false;
    let mut length = 0usize;
    for b in bytes {
        if string {
            length += 1;
            if length > 8192 {
                return Err(VideoError::Limit);
            }
            if escape {
                escape = false
            } else if *b == b'\\' {
                escape = true
            } else if *b == b'"' {
                string = false
            }
        } else {
            if matches!(*b, b'{' | b'[' | b',' | b':') {
                tokens += 1;
                if tokens > 4096 {
                    return Err(VideoError::Limit);
                }
            }
            match *b {
                b'"' => {
                    string = true;
                    length = 0
                }
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 16 {
                        return Err(VideoError::Limit);
                    }
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1).ok_or(VideoError::InvalidMetadata)?;
                }
                b':' => {
                    fields += 1;
                    if fields > 512 {
                        return Err(VideoError::Limit);
                    }
                }
                _ => {}
            }
        }
    }
    if string || depth != 0 {
        return Err(VideoError::InvalidMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_bounds_and_ceiling() {
        assert_eq!(micros("1.0000001").unwrap(), 1_000_001);
        assert_eq!(micros("-0.0000001").unwrap(), 0);
        for s in ["NaN", "inf", "1e9", "", "1.2.3", "9999999999999999999999999999999999999999999"] {
            assert!(micros(s).is_err());
        }
    }
    #[test]
    fn malformed_depth_items_and_string_work_rejected_before_tree() {
        assert!(preflight_json(format!("{}0{}", "[".repeat(17), "]".repeat(17)).as_bytes()).is_err());
        assert!(preflight_json(format!("[{}]", "0,".repeat(4097)).as_bytes()).is_err());
        assert!(preflight_json(format!("\"{}\"", "x".repeat(8193)).as_bytes()).is_err());
        assert!(preflight_json(br#"{"ok":[1,2]}"#).is_ok());
    }
    #[test]
    fn mov_external_references_always_disabled() {
        let args = input_args(true);
        let args: Vec<_> = args.iter().map(|s| s.to_string_lossy()).collect();
        for flag in ["-enable_drefs", "-use_absolute_path"] {
            let i = args.iter().position(|s| s == flag).unwrap();
            assert_eq!(args.get(i + 1).unwrap(), "0");
        }
        assert!(args.windows(2).any(|a| a == ["-protocol_whitelist", "file,pipe"]));
    }
    fn metadata() -> Value {
        serde_json::json!({"streams":[{"index":0,"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","width":16,"height":8,"time_base":"1/1000","start_pts":5000,"start_time":"5","duration_ts":3000,"duration":"3","sample_aspect_ratio":"2:1","color_space":"bt709","color_primaries":"bt709","color_transfer":"bt709","color_range":"tv"}],"format":{"start_time":"0","duration":"8"}})
    }
    #[test]
    fn duration_ticks_color_fallback_and_conflicting_rotations_are_explicit() {
        let mut value = metadata();
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).unwrap().duration_us, 3_000_000);
        value["streams"][0]["duration"] = "30".into();
        assert!(matches!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared), Err(VideoError::InvalidMetadata)));
        let mut value = metadata();
        value["streams"][0].as_object_mut().unwrap().remove("color_space");
        value["streams"][0]["color_primaries"] = "smpte170m".into();
        value["streams"][0]["color_transfer"] = "smpte170m".into();
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::default()).unwrap().color.profile, ColorProfile::Smpte170m);
        value["streams"][0]["color_transfer"] = "smpte2084".into();
        assert!(matches!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::default()), Err(VideoError::Unsupported)));
        let mut value = metadata();
        value["streams"][0]["side_data_list"] = serde_json::json!([
            {"rotation":90,"displaymatrix":"0: 0 -65536 0\n1: 65536 0 0\n2: 0 0 1073741824"},
            {"rotation":0,"displaymatrix":"0: 65536 0 0\n1: 0 65536 0\n2: 0 0 1073741824"}
        ]);
        assert!(matches!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared), Err(VideoError::InvalidMetadata)));
        value["streams"][0]["side_data_list"][0]["rotation"] = i64::MIN.into();
        assert!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).is_err());
    }
    #[test]
    fn hostile_format_duration_subtraction_returns_error_instead_of_overflow() {
        let mut value = metadata();
        let stream = value["streams"][0].as_object_mut().unwrap();
        stream.remove("duration");
        stream.remove("duration_ts");
        for duration in [
            "-170141183460469231731687303715884",
            "170141183460469231731687303715884",
            "-99999999999999999999999999999999999999999",
            "99999999999999999999999999999999999999999",
            "-1",
            "0",
            "4.999999",
        ] {
            value["format"]["duration"] = duration.into();
            assert!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).is_err());
        }
        value["format"]["duration"] = "8".into();
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).unwrap().duration_us, 3_000_000);
    }
    #[test]
    fn only_known_audio_codecs_are_kept() {
        let mut value = metadata();
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).unwrap().audio, None);
        let streams = value["streams"].as_array_mut().unwrap();
        streams.push(serde_json::json!({"index":1,"codec_type":"audio","codec_name":"ac3"}));
        streams.push(serde_json::json!({"index":2,"codec_type":"audio","codec_name":"aac"}));
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).unwrap().audio, None);
        value["streams"][1]["codec_name"] = "opus".into();
        assert_eq!(parse(&serde_json::to_vec(&value).unwrap(), ColorPolicy::RequireDeclared).unwrap().audio, Some((1, "opus")));
    }
    #[test]
    fn sound_files_take_their_first_decodable_audio_stream() {
        let json = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"mjpeg"},{"index":1,"codec_type":"audio","codec_name":"pcm_s16le","sample_rate":"44100","channels":2}],"format":{"duration":"2.500000"}}"#;
        let m = parse_audio(json).unwrap();
        assert_eq!((m.duration_us, m.stream, m.codec, m.stream_start), (2_500_000, 1, "pcm_s16le", 0), "cover art ignored, raw start at 0");
        let alac = br#"{"streams":[{"index":0,"codec_type":"audio","codec_name":"alac","sample_rate":"44100","channels":2,"duration":"1.0"}],"format":{}}"#;
        assert_eq!(parse_audio(alac).err(), Some(VideoError::Unsupported));
        let surround = br#"{"streams":[{"index":0,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":12,"duration":"1.0"}],"format":{}}"#;
        assert_eq!(parse_audio(surround).err(), Some(VideoError::Unsupported));
        let silent = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}],"format":{"duration":"1.0"}}"#;
        assert_eq!(parse_audio(silent).err(), Some(VideoError::Unsupported));
        assert_eq!(audio_demuxer("audio/mpeg"), Some("mp3"));
        assert_eq!(audio_demuxer("image/png"), None);
        assert!(audio_input_args("mov").iter().any(|a| a == "-enable_drefs"));
    }
}
