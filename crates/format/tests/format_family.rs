use nodecraft_types::ErrorKind;
use std::io::{Cursor, Write};

#[test]
fn streaming_family_is_unsupported_before_graph_or_checkpoint_parsing() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("project.json", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
    zip.write_all(br#"{"version":4,"project":{"future":"not a graph"}}"#).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    assert_eq!(nodecraft_format::decode(&bytes).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(nodecraft_format::journal::recover(b"NCRJ0004").err().unwrap().kind, ErrorKind::Unsupported);
}

#[test]
#[ignore = "cross-writer check; NODECRAFT_FORMAT_FAMILY_EVIDENCE points to actual streaming writer output"]
fn actual_streaming_writer_output_is_unsupported() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_FORMAT_FAMILY_EVIDENCE").unwrap());
    let archive = std::fs::read(folder.join("streaming-v4.ncraft")).unwrap();
    let journal = std::fs::read(folder.join("streaming-v4.ncrj")).unwrap();
    assert_eq!(nodecraft_format::decode(&archive).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(nodecraft_format::journal::recover(&journal).err().unwrap().kind, ErrorKind::Unsupported);
}
