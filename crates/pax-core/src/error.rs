//! Error types for the core domain vocabulary.
//!
//! These errors are intentionally small and value-oriented: they describe
//! *parsing and validation* failures of domain types. Operational failures
//! (timeouts, backend errors, transfer aborts) live in the higher-level crates
//! such as `pax-transport`, which wrap [`Error`] where relevant.

use core::fmt;

/// A core-domain error: almost always a failed parse or an out-of-range value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A Bluetooth address string could not be parsed into a [`crate::BdAddr`].
    ///
    /// The contained string is the offending input, echoed back to aid logging.
    InvalidAddress(String),

    /// A typed value failed validation.
    ///
    /// `what` names the type (e.g. `"HCI version"`), `detail` explains why.
    Invalid {
        /// The name of the thing being validated.
        what: &'static str,
        /// A human-readable explanation of the failure.
        detail: String,
    },
}

impl Error {
    /// Construct an [`Error::Invalid`] with a borrowed label and owned detail.
    ///
    /// ```
    /// # use pax_core::error::Error;
    /// let e = Error::invalid("HCI version", "0xFF is not assigned");
    /// assert_eq!(e.to_string(), "invalid HCI version: 0xFF is not assigned");
    /// ```
    pub fn invalid(what: &'static str, detail: impl Into<String>) -> Self {
        Error::Invalid {
            what,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidAddress(s) => write!(f, "invalid Bluetooth address: {s}"),
            Error::Invalid { what, detail } => write!(f, "invalid {what}: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias for results carrying a core [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
