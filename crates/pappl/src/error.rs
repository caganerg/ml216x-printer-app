// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The error type crossing the wrapper boundary.

use std::fmt;

/// What went wrong on the Rust side of a PAPPL callback.
///
/// PAPPL's callbacks report failure as `false` (or a null pointer); there is no
/// channel for a message, so the wrapper logs the detail through
/// `papplLogJob` and returns the bare failure. This type is what carries the
/// detail as far as that log line.
#[derive(Debug)]
pub enum Error {
    /// The C side handed us a null pointer where an object was required.
    NullPointer(&'static str),
    /// A string from C was not valid UTF-8.
    NotUtf8(&'static str),
    /// A string on its way to C contained an interior NUL byte.
    InteriorNul(&'static str),
    /// A device write returned short or negative.
    DeviceWrite {
        /// What we asked PAPPL to write.
        requested: usize,
        /// What `papplDeviceWrite` reported; negative means the device failed.
        returned: isize,
    },
    /// The job was cancelled while we were producing output for it.
    Cancelled,
    /// An I/O error while emitting a callback result.
    Io(std::io::Error),
    /// A failure produced by the driver itself, with its own message.
    Driver(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NullPointer(what) => write!(f, "PAPPL passed a null {what}"),
            Error::NotUtf8(what) => write!(f, "{what} from PAPPL is not valid UTF-8"),
            Error::InteriorNul(what) => write!(f, "{what} contains an interior NUL byte"),
            Error::DeviceWrite {
                requested,
                returned,
            } => write!(
                f,
                "device write failed: asked for {requested} bytes, papplDeviceWrite returned {returned}"
            ),
            Error::Io(error) => write!(f, "I/O error: {error}"),
            Error::Cancelled => write!(f, "the job was cancelled"),
            Error::Driver(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        let kind = match error {
            Error::Cancelled => std::io::ErrorKind::Interrupted,
            Error::DeviceWrite { .. } => std::io::ErrorKind::BrokenPipe,
            _ => std::io::ErrorKind::InvalidData,
        };
        std::io::Error::new(kind, error.to_string())
    }
}

/// Result alias for the wrapper.
pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
