//! Codec backends. Each module is gated by a Cargo feature; see `Cargo.toml`
//! for the tier layout.
//!
//! Rule: nothing in this crate may pull in an AGPL dependency. `cargo deny`
//! enforces it. AGPL backends belong in a separate `sqzer-codecs-agpl` crate.

use sqzer_core::codec::{Decoder, Encoder};

/// Registry of every backend compiled into this build.
#[derive(Default)]
pub struct Registry {
    decoders: Vec<Box<dyn Decoder>>,
    encoders: Vec<Box<dyn Encoder>>,
}

impl Registry {
    /// Registry with every backend enabled by the active feature set.
    #[must_use]
    pub fn from_features() -> Self {
        Self::default()
    }

    /// Add a decoder.
    pub fn register_decoder(&mut self, d: Box<dyn Decoder>) {
        self.decoders.push(d);
    }

    /// Add an encoder.
    pub fn register_encoder(&mut self, e: Box<dyn Encoder>) {
        self.encoders.push(e);
    }

    /// Compiled-in decoders.
    #[must_use]
    pub fn decoders(&self) -> &[Box<dyn Decoder>] {
        &self.decoders
    }

    /// Compiled-in encoders.
    #[must_use]
    pub fn encoders(&self) -> &[Box<dyn Encoder>] {
        &self.encoders
    }
}
