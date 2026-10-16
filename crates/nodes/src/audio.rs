use crate::SourceKind;
use nodecraft_types::Modality;

pub fn kind() -> SourceKind {
    SourceKind::new("source.audio", "Audio", "A", Modality::Audio)
}
