use super::*;
use nodecraft_types::MediaMeta;
use std::io::{Cursor, Read};

fn asset(store: &mut AssetStore, bytes: &[u8], mime: &str) -> AssetRef {
    let hash = store.insert(bytes).unwrap();
    AssetRef { hash, mime: mime.parse().unwrap(), meta: MediaMeta::default() }
}

fn bundle(media: Vec<(String, AssetRef)>) -> Bundle {
    Bundle { document: "Cut.fcpxml".into(), xml: "<fcpxml/>".into(), report: "Nothing left out.\n".into(), subtitles: None, media }
}

#[test]
fn the_archive_holds_the_document_the_report_and_each_media_file() {
    let mut store = AssetStore::default();
    let fox = asset(&mut store, b"fox video", "text/plain");
    let owl = asset(&mut store, b"owl sound", "text/plain");
    let b = bundle(vec![("media/01-fox.mp4".into(), fox), ("media/02-éule.wav".into(), owl)]);
    let out = b.write_to(&store, Cursor::new(Vec::new()), &TransferCancel::default()).unwrap().into_inner();
    let mut zip = zip::ZipArchive::new(Cursor::new(out)).unwrap();
    let names: Vec<String> = zip.file_names().map(|name| name.unwrap().into_owned()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for (name, expected) in
        [("Cut.fcpxml", &b"<fcpxml/>"[..]), (REPORT, b"Nothing left out.\n"), ("media/01-fox.mp4", b"fox video"), ("media/02-éule.wav", b"owl sound")]
    {
        let mut entry = zip.by_name(name).unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected, "{name}");
    }
}

#[test]
fn unsafe_or_repeated_names_are_refused() {
    let mut store = AssetStore::default();
    let fox = asset(&mut store, b"fox", "text/plain");
    for bad in ["../fox.mp4", "/fox.mp4", "media//fox.mp4", "media/./fox.mp4", "C:fox.mp4", "media\\fox.mp4", "a\u{0}b", ""] {
        let b = bundle(vec![(bad.into(), fox.clone())]);
        assert!(b.check().is_err(), "{bad:?}");
    }
    assert!(bundle(vec![(REPORT.into(), fox.clone())]).check().is_err(), "the report's name is taken");
    assert!(
        bundle(vec![("media/a.mp4".into(), fox.clone()), ("media/A.mp4".into(), fox.clone())]).check().is_err(),
        "names differing in case collide on some disks"
    );
    assert!(bundle(vec![("x".repeat(MAX_ENTRY_NAME_BYTES + 1), fox.clone())]).check().is_err());
    let many = (0..=MAX_MEDIA).map(|n| (format!("media/{n}.mp4"), fox.clone())).collect();
    assert!(bundle(many).check().is_err());
}

#[test]
fn media_that_isnt_here_is_refused_before_anything_is_written() {
    let store = AssetStore::default();
    let mut elsewhere = AssetStore::default();
    let missing = asset(&mut elsewhere, b"not in this store", "text/plain");
    let b = bundle(vec![("media/01.mp4".into(), missing)]);
    assert!(b.write_to(&store, Cursor::new(Vec::new()), &TransferCancel::default()).is_err());
}

#[test]
fn a_cancelled_export_stops() {
    let mut store = AssetStore::default();
    let fox = asset(&mut store, b"fox", "text/plain");
    let b = bundle(vec![("media/01.mp4".into(), fox)]);
    let cancel = TransferCancel::default();
    cancel.cancel();
    assert!(b.write_to(&store, Cursor::new(Vec::new()), &cancel).is_err());
}

#[test]
fn progress_counts_only_copied_media_and_mid_copy_cancellation_is_observed() {
    let mut store = AssetStore::default();
    let bytes = vec![42; 3 * crate::asset_source::TRANSFER_BYTES];
    let a = asset(&mut store, &bytes, "text/plain");
    let b = bundle(vec![("media/first.bin".into(), a.clone()), ("media/second.bin".into(), a)]);
    let mut reports = Vec::new();
    b.write_controlled(&store, Cursor::new(Vec::new()), &TransferCancel::default(), &mut |p| reports.push(p)).unwrap();
    assert_eq!(reports.first().unwrap(), &Progress { total_media: 2, total_bytes: bytes.len() as u64 * 2, ..Default::default() });
    assert_eq!(
        reports.last().unwrap(),
        &Progress { completed_media: 2, total_media: 2, copied_bytes: bytes.len() as u64 * 2, total_bytes: bytes.len() as u64 * 2, verifying: false }
    );
    assert!(reports.windows(2).all(|pair| pair[0].copied_bytes <= pair[1].copied_bytes));
    let cancel = TransferCancel::default();
    let mut latest = Progress::default();
    let error = b
        .write_controlled(&store, Cursor::new(Vec::new()), &cancel, &mut |p| {
            latest = p;
            if p.copied_bytes > 0 {
                cancel.cancel();
            }
        })
        .unwrap_err();
    assert!(TransferCancel::is_cancellation(&error));
    assert_eq!(latest.completed_media, 0);
    assert!(latest.copied_bytes < latest.total_bytes);
}

#[test]
fn srt_companion_bytes_are_in_the_archive_and_obey_all_entry_limits() {
    let store = AssetStore::default();
    let text = "1\n00:00:01,000 --> 00:00:02,000\nFüchse\n\n";
    let mut b = bundle(vec![]);
    b.subtitles = Some(("Cut.srt".into(), text.into()));
    let out = b.write_to(&store, Cursor::new(Vec::new()), &TransferCancel::default()).unwrap().into_inner();
    let mut zip = zip::ZipArchive::new(Cursor::new(out)).unwrap();
    assert_eq!(zip.len(), 3);
    let mut actual = String::new();
    zip.by_name("Cut.srt").unwrap().read_to_string(&mut actual).unwrap();
    assert_eq!(actual, text);
    for bad in ["../Cut.srt", "Cut.fcpxml", "REPORT.TXT", "C:Cut.srt"] {
        b.subtitles = Some((bad.into(), text.into()));
        assert!(b.check().is_err(), "{bad}");
    }
    b.subtitles = Some(("Cut.srt".into(), "x".repeat(MAX_TEXT_BYTES + 1)));
    assert!(b.check().is_err());
}
