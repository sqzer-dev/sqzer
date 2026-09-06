//! The set of backends compiled into a build, and format dispatch over it.

use crate::codec::{Decoder, Encoder, Format, FormatInfo};
use crate::image::Image;
use crate::params::DecodeOpts;
use crate::{Error, Result};

/// Backends available to a pipeline.
///
/// Precedence: when several encoders claim the same format, the most
/// recently registered one wins. `sqzer-codecs` registers the portable tier
/// first and the native tier second, so an opted-in C backend takes over
/// its format, and a user registering an AGPL backend afterwards takes over
/// again.
#[derive(Default)]
pub struct Registry {
    decoders: Vec<Box<dyn Decoder>>,
    encoders: Vec<Box<dyn Encoder>>,
}

/// A decoded image together with what it was decoded from.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// The image.
    pub image: Image,
    /// Detected input format.
    pub info: FormatInfo,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a decoder.
    pub fn register_decoder<D: Decoder + 'static>(&mut self, d: D) -> &mut Self {
        self.decoders.push(Box::new(d));
        self
    }

    /// Add an encoder. See the type docs for precedence.
    pub fn register_encoder<E: Encoder + 'static>(&mut self, e: E) -> &mut Self {
        self.encoders.push(Box::new(e));
        self
    }

    /// Compiled-in decoders, in registration order.
    pub fn decoders(&self) -> impl DoubleEndedIterator<Item = &dyn Decoder> {
        self.decoders.iter().map(AsRef::as_ref)
    }

    /// Compiled-in encoders, in registration order.
    pub fn encoders(&self) -> impl DoubleEndedIterator<Item = &dyn Encoder> {
        self.encoders.iter().map(AsRef::as_ref)
    }

    /// Ask every decoder to sniff `bytes`; first match wins.
    #[must_use]
    pub fn probe(&self, bytes: &[u8]) -> Option<(FormatInfo, &dyn Decoder)> {
        self.decoders()
            .find_map(|d| d.probe(bytes).map(|info| (info, d)))
    }

    /// Probe and decode.
    ///
    /// # Errors
    /// [`Error::UnknownFormat`] if nothing recognises the bytes, else
    /// whatever the decoder returns.
    pub fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Decoded> {
        let (info, decoder) = self.probe(bytes).ok_or(Error::UnknownFormat)?;
        let image = decoder.decode(bytes, opts)?;
        Ok(Decoded { image, info })
    }

    /// The encoder that currently owns `format`.
    ///
    /// # Errors
    /// [`Error::EncoderUnavailable`], naming the features that would add one.
    pub fn encoder(&self, format: Format) -> Result<&dyn Encoder> {
        self.encoders()
            .rev()
            .find(|e| e.caps().format == format)
            .ok_or(Error::EncoderUnavailable {
                format,
                available_in: format.encoder_features(),
            })
    }

    /// Whether any encoder claims `format`.
    #[must_use]
    pub fn has_encoder(&self, format: Format) -> bool {
        self.encoders().any(|e| e.caps().format == format)
    }
}

impl core::fmt::Debug for Registry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let decoders: Vec<_> = self.decoders().map(|d| d.caps().format).collect();
        let encoders: Vec<_> = self.encoders().map(|e| e.caps().format).collect();
        f.debug_struct("Registry")
            .field("decoders", &decoders)
            .field("encoders", &encoders)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{DecoderCaps, EncoderCaps, Tier};
    use crate::params::EncodeParams;

    struct FakeEncoder(EncoderCaps, &'static [u8]);

    impl Encoder for FakeEncoder {
        fn caps(&self) -> &EncoderCaps {
            &self.0
        }
        fn encode(&self, _: &Image, _: &EncodeParams) -> Result<Vec<u8>> {
            Ok(self.1.to_vec())
        }
    }

    fn caps(format: Format, tier: Tier) -> EncoderCaps {
        EncoderCaps {
            format,
            lossy: true,
            lossless: false,
            alpha: false,
            animation: false,
            bit_depth: &[8],
            hdr: false,
            quality_range: 0.0..=100.0,
            effort_range: 0..=10,
            tier,
        }
    }

    struct FakeDecoder(DecoderCaps);

    impl Decoder for FakeDecoder {
        fn caps(&self) -> &DecoderCaps {
            &self.0
        }
        fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
            bytes.starts_with(b"FAKE").then_some(FormatInfo {
                format: self.0.format,
                animated: false,
            })
        }
        fn decode(&self, _: &[u8], _: &DecodeOpts) -> Result<Image> {
            Image::from_u8(1, 1, crate::image::ColorType::Gray, vec![0])
        }
    }

    #[test]
    fn missing_encoder_names_features() {
        let reg = Registry::new();
        match reg.encoder(Format::WebP) {
            Err(Error::EncoderUnavailable {
                format: Format::WebP,
                available_in,
            }) => assert_eq!(available_in, &["webp-lossless", "native-webp"]),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("unexpected encoder"),
        }
    }

    #[test]
    fn last_registered_encoder_wins() {
        let mut reg = Registry::new();
        reg.register_encoder(FakeEncoder(caps(Format::Jpeg, Tier::Portable), b"portable"));
        reg.register_encoder(FakeEncoder(caps(Format::Jpeg, Tier::Native), b"native"));
        let img = Image::from_u8(1, 1, crate::image::ColorType::Gray, vec![0]).unwrap();
        let out = reg
            .encoder(Format::Jpeg)
            .unwrap()
            .encode(&img, &EncodeParams::default())
            .unwrap();
        assert_eq!(out, b"native");
        assert!(reg.has_encoder(Format::Jpeg));
        assert!(!reg.has_encoder(Format::Png));
    }

    #[test]
    fn decode_dispatches_on_probe() {
        let mut reg = Registry::new();
        reg.register_decoder(FakeDecoder(DecoderCaps {
            format: Format::Gif,
            animation: false,
            tier: Tier::Portable,
        }));
        assert!(matches!(
            reg.decode(b"nope", &DecodeOpts::default()),
            Err(Error::UnknownFormat)
        ));
        let out = reg.decode(b"FAKE!", &DecodeOpts::default()).unwrap();
        assert_eq!(out.info.format, Format::Gif);
        assert_eq!(out.image.pixels(), 1);
    }
}
