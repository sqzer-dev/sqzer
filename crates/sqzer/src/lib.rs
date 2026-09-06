//! `sqzer` - multi-format image optimizer with best-in-class defaults.
//!
//! ```no_run
//! use sqzer::Sqzer;
//! use sqzer::core::codec::Format;
//!
//! let out = Sqzer::new()
//!     .format(Format::Avif)
//!     .run(&std::fs::read("photo.jpg").unwrap())
//!     .unwrap();
//! std::fs::write("photo.avif", out).unwrap();
//! ```

pub use sqzer_codecs as codecs;
pub use sqzer_core as core;
pub use sqzer_metrics as metrics;

use sqzer_core::codec::Format;
use sqzer_core::params::{EncodeParams, Target};
use sqzer_core::{Error, Result};

/// One-shot builder. Cheap to create; holds no image data.
#[derive(Debug, Clone)]
pub struct Sqzer {
    format: Option<Format>,
    params: EncodeParams,
}

impl Default for Sqzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Sqzer {
    /// Builder with the `web` preset.
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: None,
            params: EncodeParams::default(),
        }
    }

    /// Output format. Defaults to a content-aware choice.
    #[must_use]
    pub fn format(mut self, f: Format) -> Self {
        self.format = Some(f);
        self
    }

    /// Perceptual target or explicit quality.
    #[must_use]
    pub fn target(mut self, t: Target) -> Self {
        self.params.target = t;
        self
    }

    /// Decode, transform, encode.
    ///
    /// # Errors
    /// Returns [`Error::EncoderUnavailable`] until backends land.
    pub fn run(&self, _input: &[u8]) -> Result<Vec<u8>> {
        Err(Error::EncoderUnavailable {
            format: self.format.unwrap_or(Format::Avif),
            available_in: &["portable"],
        })
    }
}
