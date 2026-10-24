use super::*;
use crate::video::native::tests::{service, wait};

fn metadata(stream_start: i128, format_start: i128) -> AudioMetadata {
    AudioMetadata { duration_us: 4_000_000, stream: 1, codec: "flac", format_start, stream_start, sample_rate: 44_100, channels: 2 }
}

fn text(args: &[std::ffi::OsString]) -> Vec<String> {
    args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let at = args.iter().position(|a| a == flag).unwrap();
    &args[at + 1]
}

#[test]
fn the_waveform_rate_fits_the_sample_cap_and_refuses_overlong_sounds() {
    assert_eq!(waveform_rate(0).unwrap(), 8_000);
    assert_eq!(waveform_rate(60_000_000).unwrap(), 8_000);
    let hour = waveform_rate(3_600_000_000).unwrap();
    assert!((1_000..8_000).contains(&hour), "{hour}");
    assert!(expected_samples(3_600_000_000, hour) * 2 <= 32 * 1024 * 1024);
    assert!(waveform_rate(MAX_WAVEFORM_US).unwrap() >= 1_000);
    assert_eq!(waveform_rate(MAX_WAVEFORM_US + 1), Err(VideoError::Limit));
    assert_eq!(waveform_rate(u64::MAX), Err(VideoError::Limit));
}

#[test]
fn pcm_requests_are_bounded() {
    assert_eq!(pcm_frames(0), Err(VideoError::Invalid));
    assert_eq!(pcm_frames(MAX_PCM_US + 1), Err(VideoError::Invalid));
    assert_eq!(pcm_frames(u64::MAX), Err(VideoError::Invalid));
    assert_eq!(pcm_frames(1).unwrap(), 1, "never empty");
    assert_eq!(pcm_frames(1_000_000).unwrap(), 48_000);
    assert_eq!(pcm_frames(MAX_PCM_US).unwrap(), 192_000);
}

#[test]
fn peaks_take_the_loudest_sample_of_each_slice() {
    let samples: Vec<i16> = vec![0, 100, -32768, 5, 0, 0, 32767, -1];
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let w = peaks(&bytes, 4, 0).unwrap();
    assert_eq!(w.peaks, vec![1, 255, 0, 255]);
    // Fewer buckets than samples per bucket, more buckets than samples.
    assert_eq!(peaks(&bytes, 1, 0).unwrap().peaks, vec![255]);
    let wide = peaks(&bytes, 16, 0).unwrap();
    assert_eq!(wide.peaks.len(), 16);
    assert_eq!(wide.peaks[4], 255);
    assert_eq!(loudness(0), 0);
    assert_eq!(loudness(1), 1, "any sound shows");
    assert_eq!(loudness(16384), 128);
}

#[test]
fn peaks_survive_hostile_input() {
    assert_eq!(peaks(&[], 0, 0), Err(VideoError::Invalid));
    assert_eq!(peaks(&[], MAX_WAVEFORM_BUCKETS + 1, 0), Err(VideoError::Invalid));
    assert_eq!(peaks(&[], 8, 0).unwrap().peaks, vec![0; 8]);
    assert_eq!(peaks(&[], 8, u64::MAX).unwrap().peaks, vec![0; 8]);
    // A stray odd byte is ignored.
    assert_eq!(peaks(&[0xff, 0x7f, 0x12], 2, 0).unwrap().peaks, vec![255, 0]);
    // A sound that decodes short of its length leaves its end silent.
    let loud: Vec<u8> = [i16::MAX; 4].iter().flat_map(|s| s.to_le_bytes()).collect();
    assert_eq!(peaks(&loud, 4, 8).unwrap().peaks, vec![255, 255, 0, 0]);
    assert_eq!(peaks(&loud, MAX_WAVEFORM_BUCKETS, u64::MAX).unwrap().peaks.len(), MAX_WAVEFORM_BUCKETS);
}

#[test]
fn pcm_keeps_whole_frames_up_to_the_request() {
    let bytes: Vec<u8> = [1i16, -1, 2, -2, 3].iter().flat_map(|s| s.to_le_bytes()).collect();
    assert_eq!(&*pcm_samples(&bytes, 10).unwrap(), &[1, -1, 2, -2], "a part frame is dropped");
    assert_eq!(&*pcm_samples(&bytes, 1).unwrap(), &[1, -1], "no more than asked for");
    assert!(pcm_samples(&bytes[..3], 10).unwrap().is_empty());
    assert!(pcm_samples(&[], 0).unwrap().is_empty());
}

#[test]
fn arguments_map_the_stream_and_seek_from_where_it_starts() {
    let path = Path::new("/scratch/asset-1");
    let args = text(&pcm_args("flac", &metadata(250_000, 50_000), path, 1_500_000, 2_000_000));
    assert_eq!(after(&args, "-ss"), "1.700000", "the stream's own start is added");
    assert_eq!(after(&args, "-t"), "2.000000");
    assert_eq!(after(&args, "-map"), "0:1");
    assert_eq!(after(&args, "-ac"), "2");
    assert_eq!(after(&args, "-ar"), "48000");
    assert_eq!(after(&args, "-f"), "flac", "the demuxer is named before the input");
    assert_eq!(args.last().map(String::as_str), Some("pipe:1"));
    assert!(args.iter().position(|a| a == "-ss") < args.iter().position(|a| a == "-i"), "the input seeks");
    let early = text(&pcm_args("wav", &metadata(0, 1_000_000), path, 0, 1));
    assert_eq!(after(&early, "-ss"), "0.000000", "a stream before the file's start seeks from 0");
    let mono = text(&pcm_args("wav", &AudioMetadata { channels: 1, ..metadata(0, 0) }, path, 0, 1));
    assert_eq!(after(&mono, "-af"), "pan=stereo|c0=c0|c1=c0", "a mono sound plays at full level on both sides");
    assert!(!mono.iter().any(|a| a == "-ac"));
    let wave = text(&waveform_args("mp3", &metadata(0, 0), path, 4000));
    assert_eq!(after(&wave, "-ac"), "1");
    assert_eq!(after(&wave, "-ar"), "4000");
    assert!(!wave.iter().any(|a| a == "-ss"));
}

/// 4 s: a half-scale 440 Hz tone, then silence, each for a second, twice.
fn pattern() -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-sound-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("pattern.wav");
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let source = "aevalsrc=exprs='if(lt(mod(t\\,2)\\,1)\\,0.5*sin(2*PI*440*t)\\,0)':d=4:s=44100";
    let args = process::args(&["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", source, "-c:a", "pcm_s16le"]);
    let status = std::process::Command::new(tool).args(&args).arg(&path).status().unwrap();
    assert!(status.success());
    let bytes = fs::read(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    bytes
}

fn peak(samples: &[i16]) -> u16 {
    samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0)
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_waveform_shows_tone_and_silence() {
    let service = service();
    let mut job = service.import_audio(Arc::from(pattern())).ok().unwrap();
    let sound = wait(&mut job).unwrap();
    let mut job = service.waveform(&sound, 8).unwrap();
    let w = wait(&mut job).unwrap();
    assert_eq!(w.peaks.len(), 8);
    for (i, p) in w.peaks.iter().enumerate() {
        if (i / 2) % 2 == 0 {
            assert!((110..=140).contains(p), "slice {i} is a half-scale tone: {:?}", w.peaks);
        } else {
            assert!(*p < 8, "slice {i} is silent: {:?}", w.peaks);
        }
    }
    assert_eq!(service.waveform(&sound, 0).err(), Some(VideoError::Invalid));
    assert_eq!(service.waveform(&sound, MAX_WAVEFORM_BUCKETS + 1).err(), Some(VideoError::Invalid));
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_pcm_seeks_exactly_and_stops_at_the_end() {
    let service = service();
    let mut job = service.import_audio(Arc::from(pattern())).ok().unwrap();
    let sound = wait(&mut job).unwrap();
    // 0.9 s to 1.1 s: a tenth of a second of tone, then silence.
    let mut job = service.pcm(&sound, 900_000, 200_000).unwrap();
    let chunk = wait(&mut job).unwrap();
    assert_eq!(chunk.start_us, 900_000);
    assert_eq!(chunk.frames(), 9_600);
    let samples = chunk.samples();
    assert!(samples.as_chunks::<2>().0.iter().all(|[l, r]| l == r), "a mono sound plays on both sides");
    assert!(peak(&samples[..2 * 4_000]) > 15_000, "tone first");
    assert!(peak(&samples[2 * 5_200..]) < 300, "then silence");
    // Past the end there's nothing; near it, only what's left.
    let mut job = service.pcm(&sound, 4_000_000, 1_000_000).unwrap();
    assert!(wait(&mut job).unwrap().samples().is_empty());
    let mut job = service.pcm(&sound, 3_800_000, 1_000_000).unwrap();
    let tail = wait(&mut job).unwrap();
    assert!(tail.frames().abs_diff(9_600) < 480, "{}", tail.frames());
    assert_eq!(service.pcm(&sound, 0, 0).err(), Some(VideoError::Invalid));
    assert_eq!(service.pcm(&sound, u64::MAX, MAX_PCM_US + 1).err(), Some(VideoError::Invalid));
    let mut job = service.pcm(&sound, u64::MAX, MAX_PCM_US).unwrap();
    assert!(wait(&mut job).unwrap().samples().is_empty());
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_pcm_reads_past_an_underestimated_duration_hint() {
    let service = service();
    let mut job = service.import_audio(Arc::from(pattern())).ok().unwrap();
    let mut sound = wait(&mut job).unwrap();
    // Model the underestimated duration reported for a VBR MP3 without a duration header.
    // Force the hint so this regression does not depend on a particular ffprobe estimator.
    Arc::make_mut(&mut sound.metadata).duration_us = 1_000_000;
    let mut job = service.pcm(&sound, 2_000_000, 1_000_000).unwrap();
    let chunk = wait(&mut job).unwrap();
    assert_eq!(chunk.frames(), 48_000);
    assert!(peak(chunk.samples()) > 15_000);
    let mut job = service.pcm(&sound, MAX_AUDIO_DURATION_US, 1_000_000).unwrap();
    assert!(wait(&mut job).unwrap().samples().is_empty(), "the one-hour bound still applies");
}

/// A headerless VBR MP3 whose initial noisy section is much more expensive than its silence.
fn vbr_without_duration_header() -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-vbr666-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let mut wave = b"RIFF".to_vec();
    let data = 12u32 * 48_000 * 2;
    wave.extend_from_slice(&(36 + data).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&48_000u32.to_le_bytes());
    wave.extend_from_slice(&96_000u32.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes());
    wave.extend_from_slice(&16u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&data.to_le_bytes());
    let mut seed = 666u32;
    for frame in 0..12 * 48_000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let sample = if frame < 96_000 { ((seed >> 16) as i16) / 2 } else { 0 };
        wave.extend_from_slice(&sample.to_le_bytes());
    }
    let input = dir.join("noise-and-silence.wav");
    let output = dir.join("no-duration-header.mp3");
    fs::write(&input, wave).unwrap();
    let mut child = std::process::Command::new(PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()))
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-c:a", "libmp3lame", "-q:a", "0", "-write_xing", "0", "-id3v2_version", "0"])
        .arg(&output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "installed ffmpeg must provide libmp3lame");
                break;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("VBR fixture creation failed or exceeded its deadline: {outcome:?}");
            }
        }
    }
    fs::read(output).unwrap()
}

#[test]
#[ignore = "explicit installed tools with libmp3lame only"]
fn actual_headerless_vbr_pcm_reaches_decoded_eof_beyond_its_duration_hint() {
    let service = service();
    let mut job = service.import_audio(Arc::from(vbr_without_duration_header())).ok().unwrap();
    let mut sound = wait(&mut job).unwrap();
    eprintln!("headerless VBR duration hint: {} us", sound.metadata.duration_us);
    // ffprobe estimators vary. Retain a deterministic underestimate even if a future version
    // learns the precise length; this remains an actual headerless VBR MP3 decoder path.
    Arc::make_mut(&mut sound.metadata).duration_us = sound.metadata.duration_us.min(4_000_000);
    let mut frames = 0u64;
    for _ in 0..8 {
        let from_us = frames * 1_000_000 / 48_000;
        let mut job = service.pcm(&sound, from_us, 2_000_000).unwrap();
        let chunk = wait(&mut job).unwrap();
        frames += chunk.frames() as u64;
        if chunk.frames() < 96_000 {
            break;
        }
    }
    let decoded_us = frames * 1_000_000 / 48_000;
    eprintln!("headerless VBR decoded EOF: {decoded_us} us");
    assert!((12_000_000..12_100_000).contains(&decoded_us), "decoded EOF: {decoded_us} us");
    assert!(decoded_us > sound.metadata.duration_us);
}
