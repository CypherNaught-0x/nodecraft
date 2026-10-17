use crate::SourceKind;
use nodecraft_types::Modality;

pub fn kind() -> SourceKind {
    SourceKind::new("source.text", "Text", "T", Modality::Text)
}
