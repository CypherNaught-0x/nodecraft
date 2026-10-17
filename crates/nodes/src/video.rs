use crate::SourceKind;
use nodecraft_types::Modality;

pub fn kind() -> SourceKind {
    SourceKind::new("source.video", "Video", "V", Modality::Video)
}
