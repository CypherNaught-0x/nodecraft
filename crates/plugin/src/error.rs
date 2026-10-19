//! Errors: a stable code another implementation can match, where in the input it happened, and
//! a fixed explanation. Input values never appear in an error.
use std::fmt;

/// The stable error codes of the contract. Fixtures name the code they expect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// Input longer than its limit (bytes, entries or a declared size).
    TooLarge,
    /// Nesting deeper than the limit.
    TooDeep,
    /// Not JSON, or not UTF-8.
    Syntax,
    /// An object names the same key twice.
    DuplicateKey,
    /// A field this version doesn't define.
    UnknownField,
    /// A field the contract requires is absent.
    MissingField,
    /// A field has the wrong JSON type.
    WrongType,
    /// A number outside the contract: a float, `-0`, or beyond ±(2^53−1); or a decimal string
    /// that isn't canonical or doesn't fit.
    BadNumber,
    /// A string with a lone surrogate or an unescaped control character.
    BadString,
    /// An ID, handle or key ID outside its grammar.
    BadId,
    /// A version string or a version number this host doesn't speak.
    BadVersion,
    /// An inventory path outside the portable grammar.
    BadPath,
    /// A digest or hex value that isn't lowercase hex of the right length, or doesn't match.
    BadDigest,
    /// Two entries claim the same name (IDs, paths after case folding, keys).
    Collision,
    /// Valid input that can't be combined: protocol ranges that don't meet, a data package that
    /// asks for capabilities.
    Incompatible,
    /// A required feature or capability this host doesn't know.
    UnsupportedFeature,
    /// A dependency lock that nothing available satisfies.
    DependencyMissing,
    /// Dependencies that form a cycle.
    DependencyCycle,
    /// Dependencies nested deeper than allowed, or not data packages.
    DependencyRejected,
    /// A signature that doesn't verify.
    BadSignature,
    /// A protocol message out of order for its job.
    Sequence,
    /// Not in canonical form where the contract requires it.
    NotCanonical,
}

impl Code {
    /// The code as fixtures and other implementations spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::TooLarge => "too_large",
            Code::TooDeep => "too_deep",
            Code::Syntax => "syntax",
            Code::DuplicateKey => "duplicate_key",
            Code::UnknownField => "unknown_field",
            Code::MissingField => "missing_field",
            Code::WrongType => "wrong_type",
            Code::BadNumber => "bad_number",
            Code::BadString => "bad_string",
            Code::BadId => "bad_id",
            Code::BadVersion => "bad_version",
            Code::BadPath => "bad_path",
            Code::BadDigest => "bad_digest",
            Code::Collision => "collision",
            Code::Incompatible => "incompatible",
            Code::UnsupportedFeature => "unsupported_feature",
            Code::DependencyMissing => "dependency_missing",
            Code::DependencyCycle => "dependency_cycle",
            Code::DependencyRejected => "dependency_rejected",
            Code::BadSignature => "bad_signature",
            Code::Sequence => "sequence",
            Code::NotCanonical => "not_canonical",
        }
    }

    /// Every code, for fixture runners.
    pub const ALL: [Code; 22] = [
        Code::TooLarge,
        Code::TooDeep,
        Code::Syntax,
        Code::DuplicateKey,
        Code::UnknownField,
        Code::MissingField,
        Code::WrongType,
        Code::BadNumber,
        Code::BadString,
        Code::BadId,
        Code::BadVersion,
        Code::BadPath,
        Code::BadDigest,
        Code::Collision,
        Code::Incompatible,
        Code::UnsupportedFeature,
        Code::DependencyMissing,
        Code::DependencyCycle,
        Code::DependencyRejected,
        Code::BadSignature,
        Code::Sequence,
        Code::NotCanonical,
    ];
}

/// Longest location kept in an error; deeper locations are cut at a segment boundary.
const MAX_AT: usize = 256;

/// A contract violation: `code` at `at` (a JSON-pointer-like location, `""` for the whole
/// input), with a fixed explanation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError {
    pub code: Code,
    pub at: String,
    pub detail: &'static str,
}

impl WireError {
    pub fn new(code: Code, detail: &'static str) -> Self {
        Self { code, at: String::new(), detail }
    }

    /// The same error, located at `at`.
    pub fn at(mut self, at: &str) -> Self {
        self.at = bounded(at);
        self
    }
}

/// `at`, cut to [`MAX_AT`] bytes at a `/` so a location never ends mid-name.
fn bounded(at: &str) -> String {
    if at.len() <= MAX_AT {
        return at.to_owned();
    }
    let mut end = MAX_AT;
    while end > 0 && !at.is_char_boundary(end) {
        end -= 1;
    }
    let cut = at.get(..end).unwrap_or("");
    let cut = cut.rfind('/').and_then(|i| cut.get(..i)).unwrap_or(cut);
    format!("{cut}/…")
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.at.is_empty() {
            write!(f, "{}: {}", self.code.as_str(), self.detail)
        } else {
            write!(f, "{} at {}: {}", self.code.as_str(), self.at, self.detail)
        }
    }
}

impl std::error::Error for WireError {}

pub type Result<T> = std::result::Result<T, WireError>;

pub(crate) fn err<T>(code: Code, detail: &'static str) -> Result<T> {
    Err(WireError::new(code, detail))
}
