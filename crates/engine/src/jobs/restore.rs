//! Default restoration refusal keeps the accepted handle with its caller.
use nodecraft_providers::JobHandle;
use nodecraft_types::Error;

/// A public-safe restoration refusal owning the original accepted-request handle.
///
/// Formatting exposes only the address-validation error, never provider/key/handle data.
/// The host must retain its persisted provider/base/generation tuple, recover the handle and
/// explicitly retry restoration; refusal is not permission to delete it or submit a new job.
pub struct RestoreHandleError {
    cause: Error,
    handle: JobHandle,
}
impl RestoreHandleError {
    pub(super) fn new(cause: Error, handle: JobHandle) -> Self {
        Self { cause, handle }
    }
    /// The typed, public-safe address-validation error.
    pub fn error(&self) -> &Error {
        &self.cause
    }
    /// Recover the exact original handle without cloning its accepted-request data.
    pub fn into_handle(self) -> JobHandle {
        self.handle
    }
}
impl std::fmt::Debug for RestoreHandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreHandleError").field("cause", &self.cause).finish_non_exhaustive()
    }
}
impl std::fmt::Display for RestoreHandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.cause, f)
    }
}
impl std::error::Error for RestoreHandleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
