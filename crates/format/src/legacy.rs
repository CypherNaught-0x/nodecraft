//! V1 migration uses the landed persistence guard, frozen against reviewed08586e.
//! Keep its contextual roles (including Generation.input_hash), harmless-extension behavior,
//! and Unsupported diagnostics identical for archives and original-identity recovery frames.
pub(crate) fn validate(bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
    crate::compatibility::validate(bytes)
}
