use super::*;

/// A PCM WAV of `ms` milliseconds: 16-bit, `rate` Hz, `channels` channels, with a `LIST` chunk
/// of odd length before the samples (chunks are padded to even).
fn wav_of(ms: u32, rate: u32, channels: u16, stated: Option<u32>) -> Vec<u8> {
    let block = u32::from(channels) * 2;
    let samples = vec![0u8; usize::try_from(rate / 1000 * ms * block).unwrap()];
    let mut fmt = vec![];
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * block).to_le_bytes());
    fmt.extend_from_slice(&u16::try_from(block).unwrap().to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let mut body = b"WAVE".to_vec();
    for (id, payload, size) in [(b"fmt ", fmt.clone(), None), (b"LIST", b"odd".to_vec(), None), (b"data", samples.clone(), stated)] {
        body.extend_from_slice(id);
        body.extend_from_slice(&size.unwrap_or(u32::try_from(payload.len()).unwrap()).to_le_bytes());
        body.extend_from_slice(&payload);
        if payload.len() % 2 == 1 {
            body.push(0);
        }
    }
    [b"RIFF".to_vec(), u32::try_from(body.len()).unwrap().to_le_bytes().to_vec(), body].concat()
}

#[test]
fn a_wav_gets_its_length_from_its_data_chunk() {
    assert_eq!(probe_sound(&wav_of(1500, 8000, 2, None)), SoundInfo { duration_ms: Some(1500), sample_rate: Some(8000), channels: Some(2) });
    // A streaming writer's unknown size: the samples run to the end.
    for unknown in [0, u32::MAX] {
        assert_eq!(probe_sound(&wav_of(250, 16_000, 1, Some(unknown))).duration_ms, Some(250));
    }
}

#[test]
fn a_cut_off_or_garbage_file_has_no_length() {
    let mut cut = wav_of(1000, 8000, 1, None);
    cut.truncate(cut.len() - 100);
    assert_eq!(probe_sound(&cut).duration_ms, None, "the data chunk says more than is there");
    assert_eq!(probe_sound(&cut).sample_rate, Some(8000), "what the headers do say stays");
    assert_eq!(probe_sound(b"RIFF\0\0\0\0WAVE"), SoundInfo::default());
    assert_eq!(probe_sound(b"not audio at all"), SoundInfo::default());
    assert_eq!(probe_sound(&[]), SoundInfo::default());
}

fn flac_of(rate: u32, channels: u8, samples: u64) -> Vec<u8> {
    let mut info = vec![0u8; 34];
    let packed = (u64::from(rate) << 44) | (u64::from(channels - 1) << 41) | (15u64 << 36) | samples;
    info[10..18].copy_from_slice(&packed.to_be_bytes());
    [b"fLaC".to_vec(), vec![0x80, 0, 0, 34], info].concat()
}

#[test]
fn a_flac_gets_its_length_from_streaminfo() {
    assert_eq!(probe_sound(&flac_of(44_100, 2, 441_000)), SoundInfo { duration_ms: Some(10_000), sample_rate: Some(44_100), channels: Some(2) });
    assert_eq!(probe_sound(&flac_of(48_000, 1, 0)).duration_ms, None, "zero samples means unknown");
}

fn ogg_page(serial: u32, granule: u64, packet: &[u8]) -> Vec<u8> {
    let mut page = b"OggS\0\x02".to_vec();
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&[0; 8]);
    page.push(1);
    page.push(u8::try_from(packet.len()).unwrap());
    page.extend_from_slice(packet);
    page
}

#[test]
fn an_ogg_gets_its_length_from_its_last_granule() {
    let mut vorbis = b"\x01vorbis\0\0\0\0\x02".to_vec();
    vorbis.extend_from_slice(&22_050u32.to_le_bytes());
    let file = [ogg_page(7, 0, &vorbis), ogg_page(7, 44_100, b"audio"), ogg_page(9, 999_999, b"other stream"), ogg_page(7, u64::MAX, b"no end")].concat();
    assert_eq!(probe_sound(&file), SoundInfo { duration_ms: Some(2_000), sample_rate: Some(22_050), channels: Some(2) });

    // Opus counts at 48 kHz and skips its pre-roll.
    let mut opus = b"OpusHead\x01\x01".to_vec();
    opus.extend_from_slice(&312u16.to_le_bytes());
    opus.extend_from_slice(&16_000u32.to_le_bytes());
    let file = [ogg_page(3, 0, &opus), ogg_page(3, 48_312, b"audio")].concat();
    assert_eq!(probe_sound(&file), SoundInfo { duration_ms: Some(1_000), sample_rate: Some(16_000), channels: Some(1) });
}

/// An MPEG-1 Layer III frame header: 128 kbit/s, 44.1 kHz, stereo, no padding (417 bytes).
const MP3: [u8; 4] = [0xFF, 0xFB, 0x90, 0x00];

fn mp3_frames(n: usize) -> Vec<u8> {
    let mut frame = MP3.to_vec();
    frame.resize(417, 0);
    frame.repeat(n)
}

#[test]
fn a_constant_bitrate_mp3_gets_its_length_from_its_size() {
    // 100 frames of 1152 samples at 44.1 kHz is 2.612 s; the size at 128 kbit/s says the same.
    let mut tag = b"ID3\x04\0\0\0\0\0\x05".to_vec();
    tag.extend_from_slice(b"\xFFtag!");
    let mut file = [tag, mp3_frames(100)].concat();
    let ms = probe_sound(&file).duration_ms.unwrap();
    assert!((2_605..=2_615).contains(&ms), "{ms}");
    assert_eq!(probe_sound(&file).sample_rate, Some(44_100));
    // An ID3v1 tag at the end isn't audio.
    file.extend_from_slice(&[b"TAG".to_vec(), vec![0; 125]].concat());
    assert_eq!(probe_sound(&file).duration_ms, Some(ms));
}

#[test]
fn a_variable_bitrate_mp3_gets_its_length_from_its_xing_header() {
    let mut first = MP3.to_vec();
    first.resize(417, 0);
    first[36..40].copy_from_slice(b"Xing");
    first[40..44].copy_from_slice(&1u32.to_be_bytes());
    first[44..48].copy_from_slice(&3_828u32.to_be_bytes());
    // 3828 frames of 1152 samples at 44.1 kHz: 99.997 s, whatever the file's size.
    let file = [first, mp3_frames(3)].concat();
    assert_eq!(probe_sound(&file).duration_ms, Some(99_997));
}

#[test]
fn an_adts_aac_counts_its_frames() {
    // 44.1 kHz, stereo, 100-byte frames, one block each.
    let mut frame = vec![0xFF, 0xF1, 0x50, 0x80, 0, 0, 0xFC];
    let len = 100usize;
    frame[3] |= u8::try_from(len >> 11).unwrap();
    frame[4] = u8::try_from((len >> 3) & 0xFF).unwrap();
    frame[5] = u8::try_from((len & 7) << 5).unwrap();
    frame.resize(len, 0);
    // 431 frames of 1024 samples: 10.008 s, rounded up. A cut-off last frame isn't counted.
    let mut file = frame.repeat(431);
    file.extend_from_slice(&frame[..50]);
    assert_eq!(probe_sound(&file), SoundInfo { duration_ms: Some(10_008), sample_rate: Some(44_100), channels: Some(2) });
}

#[test]
fn hostile_bytes_are_unknown_not_panics() {
    let mut vorbis = b"\x01vorbis\0\0\0\0\x02".to_vec();
    vorbis.extend_from_slice(&22_050u32.to_le_bytes());
    let files = [wav_of(100, 8000, 1, None), flac_of(44_100, 2, 4410), [ogg_page(7, 0, &vorbis), ogg_page(7, 44_100, b"audio")].concat(), mp3_frames(3)];
    for good in files {
        for cut in 0..good.len().min(600) {
            let _ = probe_sound(&good[..cut]);
        }
        for byte in 0..good.len().min(600) {
            let mut bad = good.clone();
            bad[byte] ^= 0xFF;
            let _ = probe_sound(&bad);
        }
    }
    // Endless empty chunks and tags stop at their caps.
    let chunks = [b"RIFF\0\0\0\0WAVE".to_vec(), b"junk\0\0\0\0".repeat(10_000)].concat();
    assert_eq!(probe_sound(&chunks).duration_ms, None);
    let tags = b"ID3\x04\0\0\0\0\0\0".repeat(10_000);
    assert_eq!(probe_sound(&tags).duration_ms, None);
}
