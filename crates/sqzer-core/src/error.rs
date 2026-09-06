//! Error type shared by every `sqzer` crate.

use crate::codec::Format;

/// All errors `sqzer` can return.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No decoder in this build recognised the input.
    UnknownFormat,
    /// The requested output format has no encoder in this build.
    EncoderUnavailable {
        /// Requested format.
        format: Format,
        /// Cargo features that would provide it.
        available_in: &'static [&'static str],
    },
    /// The image exceeds the configured pixel limit.
    TooLarge {
        /// Pixel count of the input.
        pixels: u64,
        /// Configured limit.
        limit: u64,
    },
    /// A backend failed. The string is the backend's own message.
    Codec(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownFormat => write!(f, "unrecognised image format"),
            Self::EncoderUnavailable {
                format,
                available_in,
            } => write!(
                f,
                "no encoder for {format:?} in this build (enable one of: {})",
                available_in.join(", ")
            ),
            Self::TooLarge { pixels, limit } => {
                write!(f, "image has {pixels} pixels, limit is {limit}")
            }
            Self::Codec(msg) => write!(f, "codec error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
