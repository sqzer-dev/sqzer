//! Error type shared by every `sqzer` crate.

use crate::codec::Format;

/// All errors `sqzer` can return.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No decoder in this build recognised the input.
    UnknownFormat,
    /// The input was recognised but nothing can decode it here: either no
    /// decoder for the format is compiled in, or the ones that are need a
    /// library or an OS component this machine does not have.
    DecoderUnavailable {
        /// Detected input format.
        format: Format,
        /// Cargo features that would provide a decoder.
        available_in: &'static [&'static str],
        /// Why the compiled-in decoders cannot run, one `name: reason`
        /// clause per backend. `None` when none is compiled in.
        reason: Option<String>,
    },
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
    /// An image or buffer that does not describe a valid picture: zero
    /// dimensions, a sample buffer of the wrong length, and so on.
    InvalidInput(String),
    /// The parameters cannot be honoured as given, for example a perceptual
    /// target that reached an encoder unresolved, or a bad codec option.
    InvalidParams(String),
    /// The encoder exists but cannot do what was asked of it, for example
    /// lossless JPEG or float samples into an 8-bit codec. Never a silent
    /// fallback.
    Unsupported {
        /// Encoder format.
        format: Format,
        /// What was asked for.
        what: String,
    },
    /// A backend failed. The string is the backend's own message.
    Codec(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownFormat => write!(f, "unrecognised image format"),
            Self::DecoderUnavailable {
                format,
                available_in,
                reason,
            } => match reason {
                Some(reason) => write!(f, "no usable {format} decoder on this machine: {reason}"),
                None if available_in.is_empty() => write!(f, "no decoder exists for {format}"),
                None => write!(
                    f,
                    "no decoder for {format} in this build (enable one of: {})",
                    available_in.join(", ")
                ),
            },
            Self::EncoderUnavailable {
                format,
                available_in,
            } => {
                if available_in.is_empty() {
                    write!(f, "no encoder exists for {format}")
                } else {
                    write!(
                        f,
                        "no encoder for {format} in this build (enable one of: {})",
                        available_in.join(", ")
                    )
                }
            }
            Self::TooLarge { pixels, limit } => {
                write!(f, "image has {pixels} pixels, limit is {limit}")
            }
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::InvalidParams(msg) => write!(f, "invalid parameters: {msg}"),
            Self::Unsupported { format, what } => {
                write!(f, "{format} encoder does not support {what}")
            }
            Self::Codec(msg) => write!(f, "codec error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
