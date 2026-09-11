//! The set of backends compiled into a build, and format dispatch over it.

use crate::codec::{Decoder, Encoder, Format, FormatInfo};
use crate::image::Image;
use crate::params::DecodeOpts;
use crate::{Error, Result};

/// A format sniff with no decoder behind it: recognises a container so a
/// build without a decoder for it can say so, instead of calling the
/// bytes unknown. See [`Registry::register_sniffer`].
pub type Sniffer = fn(&[u8]) -> Option<FormatInfo>;

/// Backends available to a pipeline.
///
/// Precedence: when several encoders claim the same format, the most
/// recently registered one wins. `sqzer-codecs` registers the portable tier
/// first and the native tier second, so an opted-in C backend takes over
/// its format, and a user registering an AGPL backend afterwards takes over
/// again.
///
/// Decoders go the other way: the first registered decoder whose
/// [`Decoder::probe`] matches and whose [`Decoder::available`] says yes
/// gets the bytes. A decoder that is compiled in but cannot run on this
/// machine is skipped, and if nothing usable is left the error names it.
#[derive(Default)]
pub struct Registry {
    decoders: Vec<Box<dyn Decoder>>,
    encoders: Vec<Box<dyn Encoder>>,
    sniffers: Vec<Sniffer>,
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

    /// Add a [`Sniffer`]. It is consulted only after every decoder has
    /// passed on the bytes, so it never takes a format away from a
    /// decoder; it turns [`Error::UnknownFormat`] into
    /// [`Error::DecoderUnavailable`] for a container this build knows but
    /// cannot read.
    pub fn register_sniffer(&mut self, sniff: Sniffer) -> &mut Self {
        self.sniffers.push(sniff);
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

    /// Ask every decoder to sniff `bytes`; the first that recognises them
    /// and is [available](Decoder::available) wins. `None` when nothing
    /// usable claims the bytes, whether or not something recognised them:
    /// [`Registry::identify`] tells those apart.
    #[must_use]
    pub fn probe(&self, bytes: &[u8]) -> Option<(FormatInfo, &dyn Decoder)> {
        self.decoders().find_map(|d| {
            d.probe(bytes)
                .filter(|_| d.available().is_ok())
                .map(|info| (info, d))
        })
    }

    /// What format `bytes` are, whether or not this build can decode them.
    /// Decoders are asked first, available or not, then the sniffers.
    #[must_use]
    pub fn identify(&self, bytes: &[u8]) -> Option<FormatInfo> {
        self.decoders()
            .find_map(|d| d.probe(bytes))
            .or_else(|| self.sniffers.iter().find_map(|sniff| sniff(bytes)))
    }

    /// Probe and decode.
    ///
    /// # Errors
    /// [`Error::UnknownFormat`] if nothing recognises the bytes,
    /// [`Error::DecoderUnavailable`] if something does but no usable
    /// decoder claims them, else whatever the decoder returns.
    pub fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Decoded> {
        let mut recognised = None;
        let mut reasons = Vec::new();
        for decoder in self.decoders() {
            let Some(info) = decoder.probe(bytes) else {
                continue;
            };
            match decoder.available() {
                Ok(()) => {
                    let image = decoder.decode(bytes, opts)?;
                    return Ok(Decoded { image, info });
                }
                Err(reason) => {
                    recognised.get_or_insert(info);
                    reasons.push(format!("{}: {reason}", decoder.caps().name));
                }
            }
        }
        let info = recognised
            .or_else(|| self.sniffers.iter().find_map(|sniff| sniff(bytes)))
            .ok_or(Error::UnknownFormat)?;
        Err(Error::DecoderUnavailable {
            format: info.format,
            available_in: info.format.decoder_features(),
            reason: (!reasons.is_empty()).then(|| reasons.join("; ")),
        })
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

    /// Whether a usable decoder claims `format`. A perceptual target needs
    /// the output format decodable to score it, and not every build that
    /// can write a format can read it back. A decoder that is compiled in
    /// but [unavailable](Decoder::available) here does not count.
    #[must_use]
    pub fn has_decoder(&self, format: Format) -> bool {
        self.decoders()
            .any(|d| d.caps().format == format && d.available().is_ok())
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
            name: "fake",
            lossy: true,
            lossless: false,
            alpha: false,
            animation: false,
            bit_depth: &[8],
            hdr: false,
            quality_range: 0.0..=100.0,
            effort_range: 0..=10,
            tier,
            options: &[],
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
        fn dimensions(&self, _: &[u8]) -> Option<(u32, u32)> {
            Some((1, 1))
        }
        fn decode(&self, _: &[u8], _: &DecodeOpts) -> Result<Image> {
            Image::from_u8(1, 1, crate::image::ColorType::Gray, vec![0])
        }
    }

    /// Recognises `FAKE` like [`FakeDecoder`] but can never run.
    struct Unavailable(DecoderCaps, &'static str);

    impl Decoder for Unavailable {
        fn caps(&self) -> &DecoderCaps {
            &self.0
        }
        fn available(&self) -> core::result::Result<(), String> {
            Err(self.1.to_string())
        }
        fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
            bytes.starts_with(b"FAKE").then_some(FormatInfo {
                format: self.0.format,
                animated: false,
            })
        }
        fn dimensions(&self, _: &[u8]) -> Option<(u32, u32)> {
            None
        }
        fn decode(&self, _: &[u8], _: &DecodeOpts) -> Result<Image> {
            panic!("an unavailable decoder must never be asked to decode")
        }
    }

    fn decoder_caps(format: Format, name: &'static str) -> DecoderCaps {
        DecoderCaps {
            format,
            name,
            animation: false,
            tier: Tier::Native,
        }
    }

    fn sniff_heic(bytes: &[u8]) -> Option<FormatInfo> {
        bytes.starts_with(b"HEIC").then_some(FormatInfo {
            format: Format::Heic,
            animated: false,
        })
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
            name: "fake",
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
        assert!(reg.has_decoder(Format::Gif));
        assert!(!reg.has_decoder(Format::Png));
    }

    #[test]
    fn unavailable_decoder_is_skipped_for_the_next_one() {
        let mut reg = Registry::new();
        reg.register_decoder(Unavailable(
            decoder_caps(Format::Heic, "os"),
            "no codec pack",
        ));
        reg.register_decoder(FakeDecoder(decoder_caps(Format::Heic, "loader")));
        let (info, decoder) = reg.probe(b"FAKE!").unwrap();
        assert_eq!(info.format, Format::Heic);
        assert_eq!(decoder.caps().name, "loader");
        assert!(reg.decode(b"FAKE!", &DecodeOpts::default()).is_ok());
        assert!(reg.has_decoder(Format::Heic));
    }

    #[test]
    fn nothing_usable_names_every_reason() {
        let mut reg = Registry::new();
        reg.register_decoder(Unavailable(
            decoder_caps(Format::Heic, "os"),
            "no codec pack",
        ));
        reg.register_decoder(Unavailable(
            decoder_caps(Format::Heic, "loader"),
            "libheif.so.1 not found",
        ));
        assert!(reg.probe(b"FAKE!").is_none(), "probe hides the unusable");
        assert_eq!(reg.identify(b"FAKE!").map(|i| i.format), Some(Format::Heic));
        assert!(!reg.has_decoder(Format::Heic));
        let err = reg.decode(b"FAKE!", &DecodeOpts::default()).unwrap_err();
        match &err {
            Error::DecoderUnavailable {
                format: Format::Heic,
                available_in,
                reason: Some(reason),
            } => {
                assert_eq!(*available_in, Format::Heic.decoder_features());
                assert_eq!(reason, "os: no codec pack; loader: libheif.so.1 not found");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            "no usable HEIC decoder on this machine: os: no codec pack; loader: libheif.so.1 not found"
        );
    }

    #[test]
    fn sniffer_turns_unknown_into_unavailable() {
        let mut reg = Registry::new();
        reg.register_sniffer(sniff_heic);
        assert!(reg.probe(b"HEIC").is_none());
        assert_eq!(reg.identify(b"HEIC").map(|i| i.format), Some(Format::Heic));
        assert_eq!(reg.identify(b"nope"), None);
        let err = reg.decode(b"HEIC", &DecodeOpts::default()).unwrap_err();
        assert!(
            matches!(
                err,
                Error::DecoderUnavailable {
                    format: Format::Heic,
                    available_in: &["native-heif"],
                    reason: None,
                }
            ),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            "no decoder for HEIC in this build (enable one of: native-heif)"
        );
        assert!(matches!(
            reg.decode(b"nope", &DecodeOpts::default()),
            Err(Error::UnknownFormat)
        ));
        // A decoder that recognises the bytes is asked before any sniffer.
        reg.register_decoder(FakeDecoder(decoder_caps(Format::Heic, "loader")));
        let mut bytes = b"FAKE".to_vec();
        bytes.extend_from_slice(b"HEIC");
        assert!(reg.decode(&bytes, &DecodeOpts::default()).is_ok());
    }
}
