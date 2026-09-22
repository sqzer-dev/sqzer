//! HEIC input, ADR-0005: up to three decoders behind the one `native-heif`
//! feature, chosen by target, none of them linking `libheif`:
//!
//! ```text
//! macOS      ImageIO (`imageio`), then libheif loaded at runtime (`libheif`)
//! Windows    WIC (`wic`), then libheif loaded at runtime
//! Linux gnu  libheif loaded at runtime
//! Linux musl nothing: a static binary cannot dlopen
//! ```
//!
//! Each is a thin shell over its binding crate (`heif-imageio`,
//! `heif-wic`, `heif-dl`, the three places in the workspace that hold
//! `unsafe`). What they share, and what keeps them conformant, lives in
//! this module and in [`crate::heif`]: the brand sniff, the container
//! walk that gives every backend the same displayed size, orientation and
//! ICC profile, and [`finish`], which turns a backend's samples into an
//! [`Image`] the same way every time. Every backend returns straight
//! alpha, the container's own rotation and mirroring applied, `clap`
//! cropped, ICC from the `colr` box, `nclx` ignored.
//!
//! The one asymmetry: `libheif` applies `irot` and `imir` itself, because
//! it cannot skip them without also skipping `clap`, and the OS decoders
//! do not, so the orientation read from the container is applied here for
//! the OS decoders only. The container walk is still the single source of
//! the orientation value, and the decode tests check all three against
//! the same rotated fixture.
//!
//! What `--list-codecs` sees: [`Decoder::available`] answers from each
//! binding's cached probe, so the listing names the decoder this build has
//! and whether this machine can use it. A decode on an unavailable backend
//! returns [`Error::DecoderUnavailable`] with the same reason.
//!
//! Known limits, shared: sequences yield their primary image; `nclx`
//! colour is ignored. WIC may hand back 8-bit samples for a 10-bit source
//! (ADR-0005 D5 allows it); which pixel formats its HEIF codec offers is
//! verification item 6 of the record.

use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

use crate::heif::{self as container, Header};

/// What a binding hands back, before the shared steps.
struct Frame {
    width: u32,
    height: u32,
    channels: u8,
    samples: Samples,
    premultiplied: bool,
    /// Whether the binding already applied the container's rotation and
    /// mirroring.
    transformed: bool,
}

/// The shared tail of every HEIC decode: layout, straight alpha, the
/// container's orientation where the binding left it to us, the ICC
/// profile from the container.
fn finish(frame: Frame, header: &Header) -> Result<Image> {
    let color = match frame.channels {
        1 => ColorType::Gray,
        2 => ColorType::GrayAlpha,
        3 => ColorType::Rgb,
        4 => ColorType::Rgba,
        n => return Err(Error::Codec(format!("HEIC decoder returned {n} channels"))),
    };
    let mut image = Image::new(frame.width, frame.height, color, frame.samples)?;
    if frame.premultiplied && color.has_alpha() {
        image = unpremultiply(image);
    }
    if !frame.transformed {
        image = image.apply_orientation(header.orientation);
    }
    Ok(image.with_icc(header.icc.clone()))
}

/// The header every backend needs before it decodes. A HEIC whose
/// container the walk cannot read is a codec error, not a mystery.
fn header(bytes: &[u8]) -> Result<Header> {
    container::header(bytes)
        .ok_or_else(|| Error::Codec("HEIC container has no readable primary item".into()))
}

fn unavailable(name: &str, reason: &str) -> Error {
    Error::DecoderUnavailable {
        format: Format::Heic,
        available_in: Format::Heic.decoder_features(),
        reason: Some(format!("{name}: {reason}")),
    }
}

/// Undo premultiplied alpha so the colour channels mean the same thing as
/// in every other decoder's output.
fn unpremultiply(image: Image) -> Image {
    let (width, height, color, samples, meta) = image.into_parts();
    let ch = color.channels();
    let samples = match samples {
        Samples::U8(mut v) => {
            for px in v.chunks_exact_mut(ch) {
                let a = u32::from(px[ch - 1]);
                if a > 0 && a < 255 {
                    for c in &mut px[..ch - 1] {
                        *c = u8::try_from((u32::from(*c) * 255 + a / 2) / a).unwrap_or(u8::MAX);
                    }
                }
            }
            Samples::U8(v)
        }
        Samples::U16(mut v) => {
            for px in v.chunks_exact_mut(ch) {
                let a = u64::from(px[ch - 1]);
                if a > 0 && a < 65535 {
                    for c in &mut px[..ch - 1] {
                        *c = u16::try_from((u64::from(*c) * 65535 + a / 2) / a).unwrap_or(u16::MAX);
                    }
                }
            }
            Samples::U16(v)
        }
        float @ Samples::F32(_) => float,
    };
    Image::new(width, height, color, samples)
        .expect("same shape as before")
        .with_metadata(meta)
}

// ---------------------------------------------------------------- libheif

/// HEIC through `libheif` loaded at runtime. 8-bit sources decode to
/// `u8`, 10 and 12-bit to `u16` scaled to the full 16-bit range.
/// Monochrome files decode to [`ColorType::Gray`], an alpha plane adds an
/// alpha channel. Unavailable, and says why, when no `libheif` 1.17 or
/// later with an HEVC decoder is found: see `heif-dl` for where it looks
/// and the `SQZER_LIBHEIF` variable.
#[cfg(not(target_env = "musl"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct LibheifDecoder;

#[cfg(not(target_env = "musl"))]
static LIBHEIF_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Heic,
    name: "libheif",
    animation: false,
    tier: Tier::Native,
};

#[cfg(not(target_env = "musl"))]
impl Decoder for LibheifDecoder {
    fn caps(&self) -> &DecoderCaps {
        &LIBHEIF_CAPS
    }

    fn available(&self) -> core::result::Result<(), String> {
        heif_dl::available()
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        container::probe(bytes)
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        container::header(bytes).map(|h| (h.width, h.height))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let header = header(bytes)?;
        let raw = heif_dl::decode(bytes, opts.max_pixels).map_err(|e| match e {
            heif_dl::Error::Unavailable(reason) => unavailable(LIBHEIF_CAPS.name, &reason),
            heif_dl::Error::TooLarge { pixels, limit } => Error::TooLarge { pixels, limit },
            heif_dl::Error::Decode(msg) => Error::Codec(msg),
        })?;
        let samples = match raw.samples {
            heif_dl::Samples::U8(v) => Samples::U8(v),
            heif_dl::Samples::U16(v) => Samples::U16(v),
        };
        finish(
            Frame {
                width: raw.width,
                height: raw.height,
                channels: raw.channels,
                samples,
                premultiplied: raw.premultiplied,
                transformed: true,
            },
            &header,
        )
    }
}

// ---------------------------------------------------------------- ImageIO

/// HEIC through macOS ImageIO, the decoder every Mac since 10.13 has.
/// 8-bit sources decode to `u8`, deeper ones to `u16` through a 16-bit
/// Quartz context. Monochrome files decode to [`ColorType::Gray`]; one
/// with an alpha plane comes back as RGBA, since Quartz has no gray-plus-
/// alpha context. Unavailable on a macOS whose ImageIO does not list
/// `public.heic`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageIoDecoder;

#[cfg(target_os = "macos")]
static IMAGEIO_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Heic,
    name: "imageio",
    animation: false,
    tier: Tier::Native,
};

#[cfg(target_os = "macos")]
impl Decoder for ImageIoDecoder {
    fn caps(&self) -> &DecoderCaps {
        &IMAGEIO_CAPS
    }

    fn available(&self) -> core::result::Result<(), String> {
        heif_imageio::available()
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        container::probe(bytes)
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        container::header(bytes).map(|h| (h.width, h.height))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let header = header(bytes)?;
        let raw = heif_imageio::decode(bytes, opts.max_pixels).map_err(|e| match e {
            heif_imageio::Error::Unavailable(reason) => unavailable(IMAGEIO_CAPS.name, &reason),
            heif_imageio::Error::TooLarge { pixels, limit } => Error::TooLarge { pixels, limit },
            heif_imageio::Error::Decode(msg) => Error::Codec(msg),
        })?;
        let samples = match raw.samples {
            heif_imageio::Samples::U8(v) => Samples::U8(v),
            heif_imageio::Samples::U16(v) => Samples::U16(v),
        };
        finish(
            Frame {
                width: raw.width,
                height: raw.height,
                channels: raw.channels,
                samples,
                premultiplied: raw.premultiplied,
                transformed: false,
            },
            &header,
        )
    }
}

// -------------------------------------------------------------------- WIC

/// HEIC through the Windows Imaging Component. Needs the HEIF Image
/// Extension and the HEVC Video Extensions from the Microsoft Store; the
/// first HEIC (or `--list-codecs`) decodes a small embedded file to find
/// out, once per process. 8-bit sources decode to `u8`; whether the HEIF
/// codec offers more than 8 bits for a 10-bit source is not yet known, so
/// such a file may come back as `u8` too. Monochrome files decode to
/// [`ColorType::Gray`] when WIC reports a gray pixel format.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WicDecoder;

#[cfg(windows)]
static WIC_CAPS: DecoderCaps = DecoderCaps {
    format: Format::Heic,
    name: "wic",
    animation: false,
    tier: Tier::Native,
};

#[cfg(windows)]
impl Decoder for WicDecoder {
    fn caps(&self) -> &DecoderCaps {
        &WIC_CAPS
    }

    fn available(&self) -> core::result::Result<(), String> {
        heif_wic::available()
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        container::probe(bytes)
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        container::header(bytes).map(|h| (h.width, h.height))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let header = header(bytes)?;
        let raw = heif_wic::decode(bytes, opts.max_pixels).map_err(|e| match e {
            heif_wic::Error::Unavailable(reason) => unavailable(WIC_CAPS.name, &reason),
            heif_wic::Error::TooLarge { pixels, limit } => Error::TooLarge { pixels, limit },
            heif_wic::Error::Decode(msg) => Error::Codec(msg),
        })?;
        let samples = match raw.samples {
            heif_wic::Samples::U8(v) => Samples::U8(v),
            heif_wic::Samples::U16(v) => Samples::U16(v),
        };
        finish(
            Frame {
                width: raw.width,
                height: raw.height,
                channels: raw.channels,
                samples,
                // WIC's converter hands back straight alpha.
                premultiplied: false,
                transformed: false,
            },
            &header,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqzer_core::image::Orientation;

    fn header_with(orientation: Orientation) -> Header {
        Header {
            width: 1,
            height: 2,
            orientation,
            icc: Some(vec![9]),
        }
    }

    #[test]
    fn unpremultiply_recovers_colour() {
        let img =
            Image::from_u8(1, 2, ColorType::Rgba, vec![64, 32, 0, 128, 10, 20, 30, 255]).unwrap();
        let out = unpremultiply(img);
        assert_eq!(
            out.samples().as_u8().unwrap(),
            &[128, 64, 0, 128, 10, 20, 30, 255]
        );
    }

    #[test]
    fn finish_applies_orientation_only_where_the_binding_did_not() {
        let frame = |transformed| Frame {
            width: 2,
            height: 1,
            channels: 1,
            samples: Samples::U8(vec![1, 2]),
            premultiplied: false,
            transformed,
        };
        let header = header_with(Orientation::Rotate90);
        let os = finish(frame(false), &header).unwrap();
        assert_eq!((os.width(), os.height()), (1, 2));
        assert_eq!(os.samples().as_u8(), Some(&[1, 2][..]));
        assert_eq!(os.icc(), Some(&[9][..]));
        let lib = finish(frame(true), &header).unwrap();
        assert_eq!((lib.width(), lib.height()), (2, 1));
        assert_eq!(lib.icc(), Some(&[9][..]));
    }

    #[test]
    fn finish_straightens_alpha_and_names_bad_layouts() {
        let header = header_with(Orientation::Normal);
        let out = finish(
            Frame {
                width: 1,
                height: 1,
                channels: 4,
                samples: Samples::U8(vec![64, 32, 0, 128]),
                premultiplied: true,
                transformed: false,
            },
            &header,
        )
        .unwrap();
        assert_eq!(out.samples().as_u8(), Some(&[128, 64, 0, 128][..]));
        let err = finish(
            Frame {
                width: 1,
                height: 1,
                channels: 5,
                samples: Samples::U8(vec![0; 5]),
                premultiplied: false,
                transformed: false,
            },
            &header,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Codec(_)), "{err}");
    }
}
