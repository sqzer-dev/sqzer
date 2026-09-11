//! End-to-end checks over the native tier's encoders: the synthetic pattern
//! in, decoded back through the registry's portable decoders. Each module
//! needs its own feature and the portable decoder for its format.
//!
//! The HEIC decoder is fixture-driven and lives in `decode.rs`. Golden
//! SSIMULACRA2 scores are in `crates/sqzer/tests/golden.rs`.

#![cfg(any(
    feature = "native-webp",
    feature = "native-jxl",
    feature = "native-avif",
    feature = "native-jpegli"
))]
// Synthetic pixel data: the truncating casts are the point.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{H, IntoParams, W, assert_close, quality, test_image, test_image_u16};
use sqzer_codecs::registry;
use sqzer_core::Error;
use sqzer_core::codec::{Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{DecodeOpts, EncodeParams, Subsampling, Target};

/// The pattern with gray replicated into three channels, which is what a
/// container without a gray layout hands back.
fn gray_as_rgb(color: ColorType) -> Image {
    let gray = test_image(color);
    let ch = color.channels();
    let mut out = Vec::new();
    for px in gray.samples().as_u8().unwrap().chunks_exact(ch) {
        out.extend_from_slice(&[px[0], px[0], px[0]]);
        out.extend_from_slice(&px[1..]);
    }
    let rgb = if color.has_alpha() {
        ColorType::Rgba
    } else {
        ColorType::Rgb
    };
    Image::from_u8(W, H, rgb, out).unwrap()
}

/// A real ICC profile: the Display P3 profile embedded in the JPEG
/// fixture. libjxl validates profiles, so fake bytes will not do.
#[cfg(feature = "jpeg")]
fn p3_profile() -> Vec<u8> {
    registry()
        .decode(&common::fixture("pattern-icc.jpg"), &DecodeOpts::default())
        .unwrap()
        .image
        .icc()
        .expect("fixture carries a profile")
        .to_vec()
}

fn is_unsupported(r: Result<Vec<u8>, Error>, format: Format) -> bool {
    r.is_err_and(|e| matches!(e, Error::Unsupported { format: f, .. } if f == format))
}

// ------------------------------------------------------------------ WebP

#[cfg(all(feature = "native-webp", feature = "webp-lossless"))]
mod webp {
    use super::*;

    #[test]
    fn takes_over_the_format() {
        let reg = registry();
        let caps = reg.encoder(Format::WebP).unwrap().caps();
        assert_eq!(caps.name, "webpx");
        assert_eq!(caps.tier, Tier::Native);
        assert!(caps.lossy && caps.lossless && caps.alpha);
    }

    #[test]
    fn lossy_round_trips_close_to_the_source() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = reg
            .encoder(Format::WebP)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[12..16], b"VP8 ");
        let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::WebP);
        assert!(!decoded.info.animated);
        assert_eq!(decoded.image.color(), ColorType::Rgb);
        assert_close(&decoded.image, &src, 5.0, "q90 round trip");
    }

    #[test]
    fn lossless_is_exact_for_every_layout() {
        let reg = registry();
        let webp = reg.encoder(Format::WebP).unwrap();
        for color in [ColorType::Rgb, ColorType::Rgba] {
            let src = test_image(color);
            let bytes = webp.encode(&src, &Target::Lossless.into_params()).unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_eq!(back, src, "layout {color:?}");
        }
        for color in [ColorType::Gray, ColorType::GrayAlpha] {
            let bytes = webp
                .encode(&test_image(color), &Target::Lossless.into_params())
                .unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_eq!(back, gray_as_rgb(color), "layout {color:?}");
        }
    }

    #[test]
    fn alpha_survives_lossy_output() {
        let reg = registry();
        let src = test_image(ColorType::Rgba);
        let bytes = reg
            .encoder(Format::WebP)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.color(), ColorType::Rgba);
        // Alpha is stored losslessly at the default alpha quality.
        assert!(common::mae_channel(&back, &src, 3) < 1e-9, "alpha plane");
        assert_close(&back, &src, 5.0, "q90 rgba round trip");
    }

    #[test]
    fn sixteen_bit_is_rounded_to_eight() {
        let reg = registry();
        let bytes = reg
            .encoder(Format::WebP)
            .unwrap()
            .encode(
                &test_image_u16(ColorType::Rgba),
                &Target::Lossless.into_params(),
            )
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back, test_image(ColorType::Rgba));
    }

    #[test]
    fn icc_survives_both_modes() {
        let reg = registry();
        let webp = reg.encoder(Format::WebP).unwrap();
        let src = test_image(ColorType::Rgb).with_icc(Some(b"fake profile bytes".to_vec()));
        for params in [quality(80.0), Target::Lossless.into_params()] {
            let bytes = webp.encode(&src, &params).unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_eq!(back.icc(), src.icc());
        }
    }

    #[test]
    fn quality_orders_size() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let webp = reg.encoder(Format::WebP).unwrap();
        let low = webp.encode(&src, &quality(20.0)).unwrap().len();
        let high = webp.encode(&src, &quality(95.0)).unwrap().len();
        assert!(
            low < high,
            "q20 {low} bytes should be smaller than q95 {high}"
        );
    }

    #[test]
    fn effort_levels_all_produce_valid_output() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        for effort in [0, 5, 10] {
            for target in [Target::Quality(70.0), Target::Lossless] {
                let params = EncodeParams {
                    effort,
                    target: target.clone(),
                    ..Default::default()
                };
                let bytes = reg
                    .encoder(Format::WebP)
                    .unwrap()
                    .encode(&src, &params)
                    .unwrap();
                let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
                assert_close(&back, &src, 8.0, &format!("effort {effort} {target:?}"));
            }
        }
    }

    #[test]
    fn options_take_effect() {
        let reg = registry();
        let webp = reg.encoder(Format::WebP).unwrap();
        let src = test_image(ColorType::Rgba);
        let plain = webp.encode(&src, &quality(80.0)).unwrap();
        // The pattern's alpha has two levels, which libwebp's alpha
        // quantisation cannot reduce; a horizontal alpha ramp has 48. On
        // an image this small the alpha chunk is a few dozen bytes, so
        // the check is that the option changes the output, not its size.
        let mut ramp = src.samples().as_u8().unwrap().to_vec();
        for (i, px) in ramp.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px[3] = ((i as u32 % W) * 255 / (W - 1)) as u8;
        }
        let ramp = Image::from_u8(W, H, ColorType::Rgba, ramp).unwrap();
        let fine_alpha = webp.encode(&ramp, &quality(80.0)).unwrap();
        let rough_alpha = webp
            .encode(
                &ramp,
                &quality(80.0).with_codec_opt("webp", "alpha_quality", "0"),
            )
            .unwrap();
        assert_ne!(rough_alpha, fine_alpha, "alpha quality changed nothing");
        let back = reg
            .decode(&rough_alpha, &DecodeOpts::default())
            .unwrap()
            .image;
        assert_eq!(back.color(), ColorType::Rgba);
        let sharp = webp
            .encode(
                &src,
                &quality(80.0).with_codec_opt("webp", "sharp_yuv", "true"),
            )
            .unwrap();
        assert_ne!(sharp, plain, "sharp_yuv changed nothing");
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let reg = registry();
        let webp = reg.encoder(Format::WebP).unwrap();
        let src = test_image(ColorType::Rgb);
        assert!(is_unsupported(
            webp.encode(
                &src,
                &EncodeParams {
                    subsampling: Subsampling::S444,
                    ..quality(75.0)
                }
            ),
            Format::WebP
        ));
        // Lossless has no chroma to subsample, so the flag is moot there.
        assert!(
            webp.encode(
                &src,
                &EncodeParams {
                    subsampling: Subsampling::S444,
                    ..Target::Lossless.into_params()
                }
            )
            .is_ok()
        );
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(is_unsupported(
            webp.encode(&hdr, &quality(75.0)),
            Format::WebP
        ));
        assert!(matches!(
            webp.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        for (key, value) in [
            ("predictor", "true"),
            ("alpha_quality", "101"),
            ("sharp_yuv", "maybe"),
        ] {
            assert!(
                matches!(
                    webp.encode(&src, &quality(75.0).with_codec_opt("webp", key, value)),
                    Err(Error::InvalidParams(_))
                ),
                "webp:{key}={value}"
            );
        }
    }
}

// ------------------------------------------------------------------- JXL

#[cfg(all(feature = "native-jxl", feature = "jxl-decode"))]
mod jxl {
    use super::*;

    #[test]
    fn provides_the_format() {
        let reg = registry();
        let caps = reg.encoder(Format::Jxl).unwrap().caps();
        assert_eq!(caps.name, "gamut-jxl");
        assert_eq!(caps.tier, Tier::Native);
        assert!(caps.lossy && caps.lossless && caps.alpha);
        assert_eq!(caps.bit_depth, &[8, 16]);
    }

    #[test]
    fn lossy_round_trips_close_to_the_source() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = reg
            .encoder(Format::Jxl)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0x0A], "bare codestream by default");
        let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::Jxl);
        assert!(!decoded.info.animated);
        assert_eq!(decoded.image.color(), ColorType::Rgb);
        assert_close(&decoded.image, &src, 3.0, "q90 round trip");
    }

    #[test]
    fn lossless_is_exact_for_every_layout_and_depth() {
        let reg = registry();
        let jxl = reg.encoder(Format::Jxl).unwrap();
        for color in [
            ColorType::Gray,
            ColorType::GrayAlpha,
            ColorType::Rgb,
            ColorType::Rgba,
        ] {
            for src in [test_image(color), test_image_u16(color)] {
                let bytes = jxl.encode(&src, &Target::Lossless.into_params()).unwrap();
                let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
                assert_eq!(back, src, "layout {color:?} {:?}", src.sample_format());
            }
        }
    }

    #[test]
    fn alpha_survives_lossy_output() {
        let reg = registry();
        let src = test_image(ColorType::Rgba);
        let bytes = reg
            .encoder(Format::Jxl)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.color(), ColorType::Rgba);
        assert!(common::mae_channel(&back, &src, 3) < 2.0, "alpha plane");
        assert_close(&back, &src, 3.0, "q90 rgba round trip");
    }

    #[cfg(feature = "jpeg")]
    #[test]
    fn icc_survives_a_round_trip() {
        let reg = registry();
        let src = test_image(ColorType::Rgb).with_icc(Some(p3_profile()));
        let jxl = reg.encoder(Format::Jxl).unwrap();
        let bytes = jxl.encode(&src, &Target::Lossless.into_params()).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.icc(), src.icc(), "lossless keeps the profile");
        // A lossy encode goes through XYB, which `jxl-oxide` renders to
        // sRGB and hands back without a profile: the colours are converted,
        // not re-tagged.
        let bytes = jxl.encode(&src, &quality(90.0)).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.icc(), None, "lossy output is rendered to sRGB");
        assert_eq!(back.color(), ColorType::Rgb);
        // A profile that cannot describe the image is a backend error, not
        // a silently dropped profile.
        let fake = test_image(ColorType::Rgb).with_icc(Some(b"fake profile bytes".to_vec()));
        assert!(matches!(
            jxl.encode(&fake, &quality(90.0)),
            Err(Error::Codec(_))
        ));
    }

    #[test]
    fn container_option_changes_the_signature() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = reg
            .encoder(Format::Jxl)
            .unwrap()
            .encode(
                &src,
                &quality(90.0).with_codec_opt("jxl", "container", "true"),
            )
            .unwrap();
        assert_eq!(&bytes[4..8], b"JXL ");
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_close(&back, &src, 3.0, "container");
    }

    #[test]
    fn quality_orders_size() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let jxl = reg.encoder(Format::Jxl).unwrap();
        let low = jxl.encode(&src, &quality(20.0)).unwrap().len();
        let high = jxl.encode(&src, &quality(95.0)).unwrap().len();
        assert!(
            low < high,
            "q20 {low} bytes should be smaller than q95 {high}"
        );
    }

    #[test]
    fn effort_levels_all_produce_valid_output() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        for effort in [0, 5, 10] {
            let params = EncodeParams {
                effort,
                ..quality(80.0)
            };
            let bytes = reg
                .encoder(Format::Jxl)
                .unwrap()
                .encode(&src, &params)
                .unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_close(&back, &src, 5.0, &format!("effort {effort}"));
        }
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let reg = registry();
        let jxl = reg.encoder(Format::Jxl).unwrap();
        let src = test_image(ColorType::Rgb);
        assert!(is_unsupported(
            jxl.encode(
                &src,
                &EncodeParams {
                    subsampling: Subsampling::S420,
                    ..quality(75.0)
                }
            ),
            Format::Jxl
        ));
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(is_unsupported(
            jxl.encode(&hdr, &quality(75.0)),
            Format::Jxl
        ));
        assert!(matches!(
            jxl.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        for (key, value) in [("effort", "7"), ("container", "sometimes")] {
            assert!(
                matches!(
                    jxl.encode(&src, &quality(75.0).with_codec_opt("jxl", key, value)),
                    Err(Error::InvalidParams(_))
                ),
                "jxl:{key}={value}"
            );
        }
    }
}

// ------------------------------------------------------------------ AVIF

#[cfg(all(feature = "native-avif", feature = "avif", not(target_arch = "wasm32")))]
mod avif {
    use super::*;

    /// Decode and bring the result back to 8 bits.
    fn decode_u8(reg: &sqzer_core::Registry, bytes: &[u8]) -> Image {
        let decoded = reg.decode(bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::Avif);
        assert!(!decoded.info.animated);
        decoded.image.to_u8(Format::Avif).unwrap().into_owned()
    }

    #[test]
    fn takes_over_the_format() {
        let reg = registry();
        let caps = reg.encoder(Format::Avif).unwrap().caps();
        assert_eq!(caps.name, "libavif");
        assert_eq!(caps.tier, Tier::Native);
        assert!(caps.lossy && !caps.lossless && caps.alpha);
    }

    #[test]
    fn round_trips_close_to_the_source() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = reg
            .encoder(Format::Avif)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(&bytes[8..12], b"avif");
        let back = decode_u8(&reg, &bytes);
        assert_eq!(back.color(), ColorType::Rgb);
        assert_close(&back, &src, 4.0, "q90 round trip");
    }

    #[test]
    fn alpha_is_preserved() {
        let reg = registry();
        let src = test_image(ColorType::Rgba);
        let bytes = reg
            .encoder(Format::Avif)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        let back = decode_u8(&reg, &bytes);
        assert_eq!(back.color(), ColorType::Rgba);
        let alpha_err = common::mae_channel(&back, &src, 3);
        assert!(alpha_err < 2.0, "alpha mean absolute error {alpha_err:.2}");
        assert_close(&back, &src, 4.0, "q90 rgba round trip");
    }

    #[test]
    fn gray_stays_gray_and_gray_alpha_widens() {
        let reg = registry();
        let avif = reg.encoder(Format::Avif).unwrap();
        let bytes = avif
            .encode(&test_image(ColorType::Gray), &quality(90.0))
            .unwrap();
        let back = decode_u8(&reg, &bytes);
        assert_eq!(back.color(), ColorType::Gray, "monochrome item");
        assert_close(&back, &test_image(ColorType::Gray), 4.0, "gray");

        let bytes = avif
            .encode(&test_image(ColorType::GrayAlpha), &quality(90.0))
            .unwrap();
        let back = decode_u8(&reg, &bytes);
        assert_eq!(back.color(), ColorType::Rgba);
        assert_close(&back, &gray_as_rgb(ColorType::GrayAlpha), 4.0, "gray alpha");
    }

    #[test]
    fn sixteen_bit_is_accepted_by_conversion() {
        let reg = registry();
        let bytes = reg
            .encoder(Format::Avif)
            .unwrap()
            .encode(&test_image_u16(ColorType::Rgb), &quality(90.0))
            .unwrap();
        assert_close(
            &decode_u8(&reg, &bytes),
            &test_image(ColorType::Rgb),
            4.0,
            "16-bit source",
        );
    }

    #[test]
    fn quality_orders_size() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let avif = reg.encoder(Format::Avif).unwrap();
        let low = avif.encode(&src, &quality(20.0)).unwrap().len();
        let high = avif.encode(&src, &quality(95.0)).unwrap().len();
        assert!(
            low < high,
            "q20 {low} bytes should be smaller than q95 {high}"
        );
    }

    #[test]
    fn every_subsampling_decodes() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        for subsampling in [
            Subsampling::Auto,
            Subsampling::S444,
            Subsampling::S422,
            Subsampling::S420,
        ] {
            let params = EncodeParams {
                subsampling,
                ..quality(85.0)
            };
            let bytes = reg
                .encoder(Format::Avif)
                .unwrap()
                .encode(&src, &params)
                .unwrap();
            assert_close(
                &decode_u8(&reg, &bytes),
                &src,
                8.0,
                &format!("{subsampling:?}"),
            );
        }
    }

    #[test]
    fn effort_levels_all_produce_valid_output() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        for effort in [0, 5, 10] {
            let params = EncodeParams {
                effort,
                ..quality(70.0)
            };
            let bytes = reg
                .encoder(Format::Avif)
                .unwrap()
                .encode(&src, &params)
                .unwrap();
            assert_close(
                &decode_u8(&reg, &bytes),
                &src,
                8.0,
                &format!("effort {effort}"),
            );
        }
    }

    #[test]
    fn alpha_quality_option_takes_effect() {
        let reg = registry();
        let avif = reg.encoder(Format::Avif).unwrap();
        let src = test_image(ColorType::Rgba);
        let plain = avif.encode(&src, &quality(80.0)).unwrap();
        let rough_alpha = avif
            .encode(
                &src,
                &quality(80.0).with_codec_opt("avif", "alpha_quality", "5"),
            )
            .unwrap();
        assert!(
            rough_alpha.len() < plain.len(),
            "alpha quality changed nothing"
        );
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let reg = registry();
        let avif = reg.encoder(Format::Avif).unwrap();
        let src = test_image(ColorType::Rgb);
        assert!(is_unsupported(
            avif.encode(&src, &Target::Lossless.into_params()),
            Format::Avif
        ));
        assert!(is_unsupported(
            avif.encode(
                &src.clone().with_icc(Some(b"fake profile bytes".to_vec())),
                &quality(75.0)
            ),
            Format::Avif
        ));
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(is_unsupported(
            avif.encode(&hdr, &quality(75.0)),
            Format::Avif
        ));
        assert!(matches!(
            avif.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        for (key, value) in [
            ("bit_depth", "10"),
            ("alpha_quality", "101"),
            ("alpha_quality", "x"),
        ] {
            assert!(
                matches!(
                    avif.encode(&src, &quality(75.0).with_codec_opt("avif", key, value)),
                    Err(Error::InvalidParams(_))
                ),
                "avif:{key}={value}"
            );
        }
    }
}

// ---------------------------------------------------------------- jpegli

#[cfg(all(feature = "native-jpegli", feature = "jpeg"))]
mod jpegli {
    use super::*;

    #[test]
    fn takes_over_the_format() {
        let reg = registry();
        let caps = reg.encoder(Format::Jpeg).unwrap().caps();
        assert_eq!(caps.name, "jpegli");
        assert_eq!(caps.tier, Tier::Native);
        assert!(caps.lossy && !caps.lossless && !caps.alpha);
        assert!(caps.options.is_empty());
    }

    #[test]
    fn round_trips_close_to_the_source() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = reg
            .encoder(Format::Jpeg)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);
        // SOF2 marks a progressive frame.
        assert!(bytes.windows(2).any(|w| w == [0xFF, 0xC2]));
        let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::Jpeg);
        assert_eq!(decoded.image.color(), ColorType::Rgb);
        assert_close(&decoded.image, &src, 4.0, "q90 round trip");
    }

    #[test]
    fn gray_stays_gray() {
        let reg = registry();
        let src = test_image(ColorType::Gray);
        let bytes = reg
            .encoder(Format::Jpeg)
            .unwrap()
            .encode(&src, &quality(90.0))
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.color(), ColorType::Gray);
        assert_close(&back, &src, 3.0, "gray round trip");
    }

    #[test]
    fn quality_orders_size() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let jpeg = reg.encoder(Format::Jpeg).unwrap();
        let low = jpeg.encode(&src, &quality(30.0)).unwrap().len();
        let high = jpeg.encode(&src, &quality(95.0)).unwrap().len();
        assert!(
            low < high,
            "q30 {low} bytes should be smaller than q95 {high}"
        );
    }

    #[test]
    fn accepts_alpha_and_16_bit_by_conversion() {
        let reg = registry();
        let jpeg = reg.encoder(Format::Jpeg).unwrap();
        for color in [ColorType::Rgba, ColorType::GrayAlpha] {
            let bytes = jpeg.encode(&test_image(color), &quality(75.0)).unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_eq!(back.color(), color.without_alpha(), "{color:?}");
        }
        assert!(
            jpeg.encode(&test_image_u16(ColorType::Rgb), &quality(75.0))
                .is_ok()
        );
    }

    #[test]
    fn subsampling_is_honoured() {
        let reg = registry();
        let jpeg = reg.encoder(Format::Jpeg).unwrap();
        let src = test_image(ColorType::Rgb);
        let full = jpeg
            .encode(
                &src,
                &EncodeParams {
                    subsampling: Subsampling::S444,
                    ..quality(75.0)
                },
            )
            .unwrap();
        let sub = jpeg
            .encode(
                &src,
                &EncodeParams {
                    subsampling: Subsampling::S420,
                    ..quality(75.0)
                },
            )
            .unwrap();
        assert_ne!(full, sub);
        for bytes in [full, sub] {
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_close(&back, &src, 6.0, "subsampled round trip");
        }
    }

    #[test]
    fn icc_survives_a_round_trip() {
        let reg = registry();
        let src = test_image(ColorType::Rgb).with_icc(Some(b"fake profile bytes".to_vec()));
        let bytes = reg
            .encoder(Format::Jpeg)
            .unwrap()
            .encode(&src, &quality(80.0))
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.icc(), src.icc());
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let reg = registry();
        let jpeg = reg.encoder(Format::Jpeg).unwrap();
        let src = test_image(ColorType::Rgb);
        assert!(is_unsupported(
            jpeg.encode(&src, &Target::Lossless.into_params()),
            Format::Jpeg
        ));
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(is_unsupported(
            jpeg.encode(&hdr, &quality(75.0)),
            Format::Jpeg
        ));
        assert!(matches!(
            jpeg.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        // mozjpeg's options are not jpegli's.
        assert!(matches!(
            jpeg.encode(
                &src,
                &quality(75.0).with_codec_opt("jpeg", "progressive", "false")
            ),
            Err(Error::InvalidParams(_))
        ));
    }
}
