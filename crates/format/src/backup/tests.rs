use super::*;

/// Cheap parameters keep the tests fast; production uses RECOMMENDED.
const FAST: KdfParams = KdfParams { memory_kib: 19 * 1024, iterations: 1, lanes: 1 };

fn entry(path: &str, bytes: &[u8]) -> BackupEntry {
    BackupEntry { path: path.into(), size: bytes.len() as u64, blake3: hex(blake3::hash(bytes).as_bytes()) }
}

fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("projects/brief.ncraft", b"project bytes".to_vec()),
        ("media/big.bin", (0..200_000u32).map(|n| (n % 251) as u8).collect()),
        ("media/empty.bin", Vec::new()),
        ("settings/app.json", br#"{"theme":"dark"}"#.to_vec()),
    ]
}

fn manifest(files: &[(&str, Vec<u8>)]) -> BackupManifest {
    BackupManifest { label: "Weekly".into(), created_ms: 7, entries: files.iter().map(|(p, b)| entry(p, b)).collect() }
}

fn protection(passphrase: &str) -> Protection<'_> {
    Protection::Passphrase { passphrase, kdf: FAST, salt: [3; 16], nonce_prefix: [9; 19] }
}

fn write(protection: Protection<'_>, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut writer = BackupWriter::create(Vec::new(), protection, &manifest(files)).unwrap();
    for (_, bytes) in files {
        writer.write_entry(&mut bytes.as_slice()).unwrap();
    }
    writer.finish().unwrap()
}

fn read(bytes: &[u8], passphrase: Option<&str>) -> Result<(BackupManifest, Vec<Vec<u8>>), Error> {
    let mut input = bytes;
    let (header, header_bytes) = BackupHeader::read(&mut input)?;
    let mut reader = BackupReader::open(input, &header, header_bytes, passphrase)?;
    let manifest = reader.manifest().clone();
    let mut out = Vec::new();
    loop {
        let mut sink = Vec::new();
        if reader.next_entry(&mut sink)?.is_none() {
            break;
        }
        out.push(sink);
    }
    reader.finish()?;
    Ok((manifest, out))
}

#[test]
fn unencrypted_and_encrypted_backups_round_trip() {
    let files = files();
    for (bytes, passphrase) in [(write(Protection::Unencrypted, &files), None), (write(protection("correct horse"), &files), Some("correct horse"))] {
        let (header, _) = BackupHeader::read(&mut bytes.as_slice()).unwrap();
        assert_eq!(header.encryption.is_some(), passphrase.is_some());
        let (m, contents) = read(&bytes, passphrase).unwrap();
        assert_eq!(m, manifest(&files));
        assert_eq!(contents, files.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>());
    }
}

#[test]
fn encrypted_backups_hide_the_manifest_and_contents() {
    let files = files();
    let sealed = write(protection("pw"), &files);
    let haystack = String::from_utf8_lossy(&sealed);
    for needle in ["brief.ncraft", "Weekly", "project bytes", "theme"] {
        assert!(!haystack.contains(needle), "{needle} is visible in an encrypted backup");
    }
    let plain = write(Protection::Unencrypted, &files);
    assert!(String::from_utf8_lossy(&plain).contains("brief.ncraft"), "an unencrypted backup is readable, as labelled");
}

#[test]
fn chunk_boundaries_round_trip() {
    for size in [0, 1, BACKUP_CHUNK_BYTES - 5, BACKUP_CHUNK_BYTES - 4, BACKUP_CHUNK_BYTES, BACKUP_CHUNK_BYTES + 1, 3 * BACKUP_CHUNK_BYTES] {
        // The length and the manifest precede the entry; varying the entry hits every boundary.
        let files = vec![("a.bin", vec![7u8; size])];
        let sealed = write(protection("pw"), &files);
        assert_eq!(read(&sealed, Some("pw")).unwrap().1, vec![vec![7u8; size]], "{size}");
    }
}

#[test]
fn a_wrong_or_missing_passphrase_is_refused() {
    let sealed = write(protection("right"), &files());
    assert_eq!(read(&sealed, Some("wrong")).unwrap_err().kind, ErrorKind::Authentication);
    assert_eq!(read(&sealed, None).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(read(&sealed, Some("")).unwrap_err().kind, ErrorKind::InvalidInput);
    let sealed = write(protection("PASS-ONE-zq"), &files());
    let e = read(&sealed, Some("PASS-TWO-zq")).unwrap_err();
    assert!(!e.message.contains("PASS"), "passphrases never appear in errors");
}

#[test]
fn any_changed_byte_is_detected() {
    let files = vec![("a.bin", (0..150_000u32).map(|n| n as u8).collect::<Vec<u8>>()), ("b.txt", b"hello".to_vec())];
    for (bytes, passphrase) in [(write(Protection::Unencrypted, &files), None), (write(protection("pw"), &files), Some("pw"))] {
        let mut positions: Vec<usize> = (0..bytes.len()).step_by(997).collect();
        positions.extend([8, 10, 11, 20, 40, bytes.len() - 1, bytes.len() - 17, bytes.len() - 33]);
        for at in positions {
            let mut tampered = bytes.clone();
            tampered[at] ^= 0x40;
            assert!(read(&tampered, passphrase).is_err(), "a change at byte {at} of {} went unnoticed", bytes.len());
        }
    }
}

#[test]
fn truncation_and_trailing_data_are_detected() {
    let files = vec![("a.bin", vec![1u8; 3 * BACKUP_CHUNK_BYTES])];
    for (bytes, passphrase) in [(write(Protection::Unencrypted, &files), None), (write(protection("pw"), &files), Some("pw"))] {
        for keep in [0, 5, 11, 60, bytes.len() / 2, bytes.len() - BACKUP_CHUNK_BYTES - 16, bytes.len() - 1] {
            assert!(read(bytes.get(..keep).unwrap(), passphrase).is_err(), "truncated to {keep}");
        }
        let mut longer = bytes.clone();
        longer.extend_from_slice(b"extra");
        assert!(read(&longer, passphrase).is_err());
    }
    // Dropping a whole middle chunk breaks the counter.
    let sealed = write(protection("pw"), &files);
    let chunk = BACKUP_CHUNK_BYTES + 16;
    let mut spliced = sealed.get(..59 + chunk).unwrap().to_vec();
    spliced.extend_from_slice(sealed.get(59 + 2 * chunk..).unwrap());
    assert_eq!(read(&spliced, Some("pw")).unwrap_err().kind, ErrorKind::Authentication);
}

#[test]
fn entries_that_changed_since_listing_are_refused() {
    let files = files();
    let m = manifest(&files);
    for source in [&b"project bytez"[..], b"project", b"project bytes and more"] {
        let mut writer = BackupWriter::create(Vec::new(), Protection::Unencrypted, &m).unwrap();
        assert_eq!(writer.write_entry(&mut &source[..]).unwrap_err().kind, ErrorKind::Conflict);
    }
    let mut writer = BackupWriter::create(Vec::new(), Protection::Unencrypted, &m).unwrap();
    writer.write_entry(&mut files[0].1.as_slice()).unwrap();
    assert!(writer.finish().is_err(), "missing entries");
}

#[test]
fn unsafe_or_malformed_manifests_are_refused() {
    let ok = entry("a/b.txt", b"x");
    let long = "x".repeat(256);
    let deep = vec!["d"; 17].join("/");
    for path in ["", "/abs", "a//b", "../up", "a/../b", "./a", "a/.", "C:\\x", "a\\b", "trailing.", " lead", "sp ace ", "nul\u{0}", "é.txt", &long, &deep] {
        let m = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![BackupEntry { path: path.into(), ..ok.clone() }] };
        assert!(m.check().is_err(), "{path:?}");
    }
    let dup = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![ok.clone(), BackupEntry { path: "A/B.TXT".into(), ..ok.clone() }] };
    assert!(dup.check().is_err(), "case-insensitive duplicates");
    let nested = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![entry("a", b"x"), ok.clone()] };
    assert!(nested.check().is_err(), "a file can't also be a folder");
    let big = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![BackupEntry { size: MAX_BACKUP_ENTRY_BYTES + 1, ..ok.clone() }] };
    assert!(big.check().is_err());
    let hash = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![BackupEntry { blake3: "AB".repeat(32), ..ok.clone() }] };
    assert!(hash.check().is_err());
    let fine = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![ok, entry("a/c d-e_f.v2.json", b"y")] };
    assert!(fine.check().is_ok());
}

#[test]
fn hostile_headers_are_refused_before_any_key_derivation() {
    assert_eq!(read(b"", None).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(read(b"NOTABACKUPATALL", None).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut newer = write(Protection::Unencrypted, &files());
    newer[8] = 2;
    assert_eq!(read(&newer, None).unwrap_err().kind, ErrorKind::Unsupported);
    let sealed = write(protection("pw"), &files());
    let with_memory = |kib: u32| {
        let mut b = sealed.clone();
        b.get_mut(12..16).unwrap().copy_from_slice(&kib.to_le_bytes());
        b
    };
    // Refused while reading the header: no gigabytes are ever allocated.
    for kib in [0, 1024, 4 * 1024 * 1024, u32::MAX] {
        assert_eq!(read(&with_memory(kib), Some("pw")).unwrap_err().kind, ErrorKind::InvalidInput, "{kib}");
    }
    let mut unknown_kdf = sealed.clone();
    unknown_kdf[11] = 9;
    assert_eq!(read(&unknown_kdf, Some("pw")).unwrap_err().kind, ErrorKind::Unsupported);
    let mut unknown_flag = sealed;
    unknown_flag[10] = 7;
    assert_eq!(read(&unknown_flag, Some("pw")).unwrap_err().kind, ErrorKind::Unsupported);
    let weak = Protection::Passphrase { passphrase: "pw", kdf: KdfParams { memory_kib: 8, iterations: 1, lanes: 1 }, salt: [0; 16], nonce_prefix: [0; 19] };
    assert!(BackupWriter::create(Vec::new(), weak, &manifest(&files())).is_err(), "weak parameters can't be written either");
    assert!(KdfParams::RECOMMENDED.check().is_ok());
}

#[test]
fn a_manifest_length_beyond_the_limit_is_refused_without_allocating_it() {
    let mut plain = BACKUP_MAGIC.to_vec();
    plain.extend_from_slice(&BACKUP_VERSION.to_le_bytes());
    plain.push(0);
    plain.extend_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(read(&plain, None).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn a_failed_entry_poisons_the_writer() {
    let files = files();
    for protect in [Protection::Unencrypted, protection("pw")] {
        let mut writer = BackupWriter::create(Vec::new(), protect, &manifest(&files)).unwrap();
        assert!(writer.write_entry(&mut &b"short"[..]).is_err());
        for (_, bytes) in files.iter().skip(1) {
            assert!(writer.write_entry(&mut bytes.as_slice()).is_err(), "no more writes after a failure");
        }
        assert!(writer.finish().is_err(), "an incomplete backup can't be finished");
    }
}

#[test]
fn windows_device_names_are_refused() {
    let ok = entry("a.txt", b"x");
    for path in ["CON", "nul", "aux.txt", "com1", "LPT9.log", "prn/x", "a/Com3.json", "nul .txt"] {
        let m = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![BackupEntry { path: path.into(), ..ok.clone() }] };
        assert!(m.check().is_err(), "{path}");
    }
    for path in ["console.txt", "com.txt", "lpt10", "auxiliary", "nullable/x"] {
        let m = BackupManifest { label: "x".into(), created_ms: 0, entries: vec![BackupEntry { path: path.into(), ..ok.clone() }] };
        assert!(m.check().is_ok(), "{path}");
    }
}

#[test]
fn a_passphrase_for_an_unencrypted_backup_is_refused() {
    let plain = write(Protection::Unencrypted, &files());
    assert_eq!(read(&plain, Some("pw")).unwrap_err().kind, ErrorKind::Authentication);
    assert!(read(&plain, None).is_ok());
}

#[test]
fn the_read_ceiling_bounds_key_derivation_cost() {
    assert!(KdfParams { memory_kib: 256 * 1024, iterations: 6, lanes: 8 }.check().is_ok());
    assert!(KdfParams { memory_kib: 256 * 1024 + 1, iterations: 1, lanes: 1 }.check().is_err());
    assert!(KdfParams { memory_kib: 64 * 1024, iterations: 7, lanes: 1 }.check().is_err());
}

/// Claims more bytes than it was given room for, and is interrupted now and then.
struct Liar<'a> {
    data: &'a [u8],
    calls: usize,
}
impl Read for Liar<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.calls.is_multiple_of(3) {
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
        }
        let n = self.data.len().min(buf.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(if n == 0 { 0 } else { n + 10 })
    }
}

#[test]
fn misbehaving_readers_cannot_cause_a_panic() {
    let files = vec![("a.bin", vec![5u8; 3 * BACKUP_CHUNK_BYTES + 7])];
    let mut writer = BackupWriter::create(Vec::new(), Protection::Unencrypted, &manifest(&files)).unwrap();
    // Over-reported counts are clamped to the room given and interruptions are retried, so the
    // copy is still exact.
    writer.write_entry(&mut Liar { data: &files[0].1, calls: 0 }).unwrap();
    assert_eq!(read(&writer.finish().unwrap(), None).unwrap().1, vec![files[0].1.clone()]);
    let plain = write(Protection::Unencrypted, &files);
    let mut input = Liar { data: &plain, calls: 0 };
    let (header, bytes) = BackupHeader::read(&mut input).unwrap();
    let mut reader = BackupReader::open(input, &header, bytes, None).unwrap();
    let mut out = Vec::new();
    reader.next_entry(&mut out).unwrap();
    assert_eq!(out, files[0].1);
    reader.finish().unwrap();
}
