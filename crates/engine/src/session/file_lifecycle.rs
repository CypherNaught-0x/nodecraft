//! Host observation of actual document adoption and successful destination saves.
use std::sync::Arc;

/// Opaque identity of one adopted document, independent of its project id and file path.
/// Ordinary edits and saves keep it; successful Open/Recover and a new Session replace it.
#[derive(Clone, Debug)]
pub struct DocumentLineage(Arc<()>);

pub(super) struct FileLifecycle {
    current: DocumentLineage,
    saved: Option<DocumentLineage>,
}
impl FileLifecycle {
    pub(super) fn new() -> Self {
        Self { current: DocumentLineage(Arc::new(())), saved: None }
    }
    pub(super) fn current(&self) -> DocumentLineage {
        self.current.clone()
    }
    pub(super) fn is_current(&self, token: &DocumentLineage) -> bool {
        Arc::ptr_eq(&self.current.0, &token.0)
    }
    pub(super) fn has_saved(&self, token: &DocumentLineage) -> bool {
        self.is_current(token) && self.saved.as_ref().is_some_and(|saved| Arc::ptr_eq(&saved.0, &token.0))
    }
    pub(super) fn saved(&mut self) {
        self.saved = Some(self.current.clone());
    }
    /// The caller prepared this identity before any durable adoption.
    pub(super) fn replaced(&mut self, identity: Arc<()>) {
        self.current = DocumentLineage(identity);
        self.saved = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_unit_values_are_not_equal_document_authority() {
        let mut a = FileLifecycle::new();
        let b = FileLifecycle::new();
        let token = a.current();
        assert!(a.is_current(&token));
        assert!(!b.is_current(&token));
        assert!(!a.has_saved(&token));
        a.saved();
        assert!(a.has_saved(&token));
        a.replaced(Arc::new(()));
        assert!(!a.is_current(&token));
        assert!(!a.has_saved(&token));
        a.saved();
        assert!(!a.has_saved(&token));
        assert!(a.has_saved(&a.current()));
    }
}
