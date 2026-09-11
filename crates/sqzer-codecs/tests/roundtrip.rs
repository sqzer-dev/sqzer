//! End-to-end checks over the encoders: the synthetic pattern in, every
//! encoder out, decoded back through the registry.
//!
//! Golden SSIMULACRA2 scores per encoder live in `crates/sqzer/tests/golden.rs`,
//! where both the codecs and the metric are in scope. Here the lossy
//! encoders are checked for a bounded mean absolute error and for size
//! ordering by quality.

#![cfg(all(feature = "png", feature = "jpeg"))]
// Synthetic pixel data: the truncating casts are the point.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{H, IntoParams, W, assert_close, quality, test_image, test_image_u16};
// The portable JPEG writer itself: a native build's registry hands out
// `jpegli` for this format.
use sqzer_codecs::jpeg::MozjpegEncoder;
use sqzer_codecs::registry;
use sqzer_core::Error;
use sqzer_core::codec::Encoder;
use sqzer_core::codec::{Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{DecodeOpts, EncodeParams, Target};

#[test]
fn feature_registry_lists_the_compiled_backends() {
    let reg = registry();
    // Portable tier first, in registration order, then the native tier,
    // which takes a format over by registering after it.
    let enc: Vec<_> = reg
        .encoders()
        .map(|e| (e.caps().format, e.caps().tier))
        .collect();
    let mut expected = vec![
        (Format::Jpeg, Tier::Portable),
        (Format::Png, Tier::Portable),
    ];
    if cfg!(feature = "webp-lossless") {
        expected.push((Format::WebP, Tier::Portable));
    }
    if cfg!(feature = "avif") {
        expected.push((Format::Avif, Tier::Portable));
    }
    if cfg!(feature = "native-webp") {
        expected.push((Format::WebP, Tier::Native));
    }
    if cfg!(feature = "native-jxl") {
        expected.push((Format::Jxl, Tier::Native));
    }
    if cfg!(feature = "native-avif") {
        expected.push((Format::Avif, Tier::Native));
    }
    if cfg!(feature = "native-jpegli") {
        expected.push((Format::Jpeg, Tier::Native));
    }
    assert_eq!(enc, expected);

    // The encoder that owns each format is the last one registered.
    let owner = |f: Format| reg.encoder(f).unwrap().caps().tier;
    let tier = |native: bool| {
        if native { Tier::Native } else { Tier::Portable }
    };
    assert_eq!(owner(Format::Jpeg), tier(cfg!(feature = "native-jpegli")));
    assert_eq!(owner(Format::Png), Tier::Portable);
    if cfg!(any(feature = "webp-lossless", feature = "native-webp")) {
        assert_eq!(owner(Format::WebP), tier(cfg!(feature = "native-webp")));
    }
    if cfg!(any(feature = "avif", feature = "native-avif")) {
        assert_eq!(owner(Format::Avif), tier(cfg!(feature = "native-avif")));
    }
    assert_eq!(reg.has_encoder(Format::Jxl), cfg!(feature = "native-jxl"));

    let dec: Vec<_> = reg
        .decoders()
        .map(|d| (d.caps().format, d.caps().tier))
        .collect();
    let mut expected = vec![
        (Format::Jpeg, Tier::Portable),
        (Format::Png, Tier::Portable),
    ];
    if cfg!(feature = "webp-lossless") {
        expected.push((Format::WebP, Tier::Portable));
    }
    if cfg!(all(feature = "avif", not(target_arch = "wasm32"))) {
        expected.push((Format::Avif, Tier::Portable));
    }
    if cfg!(feature = "jxl-decode") {
        expected.push((Format::Jxl, Tier::Portable));
    }
    // HEIC: the OS decoder and the runtime loader on macOS and Windows,
    // the loader alone on Linux gnu, nothing on musl (ADR-0005).
    if cfg!(all(
        feature = "native-heif",
        any(target_os = "macos", windows)
    )) {
        expected.push((Format::Heic, Tier::Native));
    }
    if cfg!(all(feature = "native-heif", not(target_env = "musl"))) {
        expected.push((Format::Heic, Tier::Native));
    }
    assert_eq!(dec, expected);
}

#[test]
fn encoder_caps_are_truthful() {
    let reg = registry();
    for enc in reg.encoders() {
        let caps = enc.caps();
        let name = caps.format;
        assert!(caps.lossy || caps.lossless, "{name}: claims neither mode");
        assert!(!caps.bit_depth.is_empty(), "{name}: claims no bit depth");

        // Lossless claim matches what Target::Lossless does.
        let lossless = enc.encode(&test_image(ColorType::Rgb), &Target::Lossless.into_params());
        assert_eq!(lossless.is_ok(), caps.lossless, "{name}: lossless claim");

        // Lossy claim matches what an explicit quality does.
        let lossy = enc.encode(&test_image(ColorType::Rgb), &quality(60.0));
        assert!(lossy.is_ok(), "{name}: explicit quality must always encode");

        // Alpha claim: an encoder that claims alpha must get it back out.
        if caps.alpha {
            let src = test_image(ColorType::Rgba);
            let params = if caps.lossless {
                Target::Lossless.into_params()
            } else {
                quality(100.0)
            };
            let bytes = enc.encode(&src, &params).unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert!(back.has_alpha(), "{name}: alpha claimed but not preserved");
        }

        // Every claimed bit depth encodes.
        for &depth in caps.bit_depth {
            let img = match depth {
                8 => test_image(ColorType::Rgb),
                16 => Image::from_u16(2, 2, ColorType::Rgb, vec![0x1234; 12]).unwrap(),
                other => panic!("{name}: no test input for {other}-bit"),
            };
            assert!(
                enc.encode(&img, &quality(60.0)).is_ok(),
                "{name}: {depth}-bit"
            );
        }

        // Every listed option is accepted at its documented default, and a
        // key that is not listed is refused. `--codec-opt` relies on both.
        assert!(!caps.name.is_empty(), "{name}: no backend name");
        let codec = name.extension();
        let codec = if codec == "jpg" { "jpeg" } else { codec };
        for opt in caps.options {
            let params = quality(60.0).with_codec_opt(codec, opt.key, opt.default);
            let params = if caps.lossy {
                params
            } else {
                EncodeParams {
                    target: Target::Lossless,
                    ..params
                }
            };
            assert!(
                enc.encode(&test_image(ColorType::Rgb), &params).is_ok(),
                "{name}: option {codec}:{} rejects its default `{}`",
                opt.key,
                opt.default
            );
        }
        let unlisted = quality(60.0).with_codec_opt(codec, "no_such_option", "1");
        assert!(
            matches!(
                enc.encode(&test_image(ColorType::Rgb), &unlisted),
                Err(Error::InvalidParams(_))
            ),
            "{name}: an unlisted option must be InvalidParams"
        );

        // No HDR claim means float samples are refused, not mangled.
        if !caps.hdr {
            let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
            assert!(
                matches!(
                    enc.encode(&hdr, &quality(60.0)),
                    Err(Error::Unsupported { .. })
                ),
                "{name}: float input must be Unsupported"
            );
        }
    }
}

#[cfg(not(feature = "native-jxl"))]
#[test]
fn unavailable_encoder_is_reported_not_substituted() {
    let reg = registry();
    match reg.encoder(Format::Jxl) {
        Err(Error::EncoderUnavailable {
            format: Format::Jxl,
            available_in,
        }) => assert_eq!(available_in, &["native-jxl"]),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("no JXL encoder should exist in the portable tier"),
    }
}

#[test]
fn png_round_trips_every_layout_losslessly() {
    let reg = registry();
    let png = reg.encoder(Format::Png).unwrap();
    for color in [
        ColorType::Gray,
        ColorType::GrayAlpha,
        ColorType::Rgb,
        ColorType::Rgba,
    ] {
        let src = test_image(color);
        let bytes = png.encode(&src, &Target::Lossless.into_params()).unwrap();
        let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::Png);
        assert!(!decoded.info.animated);
        assert_eq!(decoded.image, src, "layout {color:?}");
    }
}

#[test]
fn png_round_trips_16_bit_and_icc() {
    let reg = registry();
    let samples: Vec<u16> = (0..(W * H * 3)).map(|i| (i * 977 % 65536) as u16).collect();
    let src = Image::new(W, H, ColorType::Rgb, Samples::U16(samples))
        .unwrap()
        .with_icc(Some(b"not really an icc profile".to_vec()));
    let bytes = reg
        .encoder(Format::Png)
        .unwrap()
        .encode(&src, &quality(50.0))
        .unwrap();
    let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
    assert_eq!(decoded, src);
}

#[test]
fn png_effort_levels_all_produce_valid_output() {
    let reg = registry();
    let src = test_image(ColorType::Rgba);
    for effort in [0, 2, 6, 10] {
        let params = EncodeParams {
            effort,
            ..quality(50.0)
        };
        let bytes = reg
            .encoder(Format::Png)
            .unwrap()
            .encode(&src, &params)
            .unwrap();
        assert_eq!(
            reg.decode(&bytes, &DecodeOpts::default()).unwrap().image,
            src
        );
    }
}

#[test]
fn pixel_limit_is_checked_from_the_header() {
    let reg = registry();
    let src = test_image(ColorType::Rgb);
    let bytes = reg
        .encoder(Format::Png)
        .unwrap()
        .encode(&src, &quality(50.0))
        .unwrap();
    let opts = DecodeOpts {
        max_pixels: u64::from(W * H) - 1,
        ..Default::default()
    };
    assert!(matches!(
        reg.decode(&bytes, &opts),
        Err(Error::TooLarge { pixels, limit }) if pixels == u64::from(W * H) && limit == pixels - 1
    ));
}

#[test]
fn unknown_bytes_are_unknown_format() {
    assert!(matches!(
        registry().decode(b"GIF89a....", &DecodeOpts::default()),
        Err(Error::UnknownFormat)
    ));
}

#[test]
fn jpeg_round_trips_close_to_the_source() {
    let reg = registry();
    let src = test_image(ColorType::Rgb);
    let jpeg = &MozjpegEncoder;

    let bytes = jpeg.encode(&src, &quality(90.0)).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);

    let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
    assert_eq!(decoded.info.format, Format::Jpeg);
    assert_eq!(decoded.image.color(), ColorType::Rgb);
    assert_close(&decoded.image, &src, 4.0, "q90 round trip");
}

#[test]
fn jpeg_quality_orders_size() {
    let src = test_image(ColorType::Rgb);
    let jpeg = &MozjpegEncoder;
    let low = jpeg.encode(&src, &quality(30.0)).unwrap().len();
    let high = jpeg.encode(&src, &quality(95.0)).unwrap().len();
    assert!(
        low < high,
        "q30 {low} bytes should be smaller than q95 {high}"
    );
}

#[test]
fn jpeg_accepts_alpha_and_16_bit_by_conversion() {
    let reg = registry();
    let jpeg = &MozjpegEncoder;
    for color in [ColorType::Rgba, ColorType::GrayAlpha, ColorType::Gray] {
        let bytes = jpeg.encode(&test_image(color), &quality(75.0)).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!((back.width(), back.height()), (W, H));
        assert_eq!(back.color(), color.without_alpha(), "{color:?}");
    }
    let wide = Image::from_u16(2, 2, ColorType::Rgb, vec![0xFFFF; 12]).unwrap();
    assert!(jpeg.encode(&wide, &quality(75.0)).is_ok());
}

#[test]
fn jpeg_refuses_what_it_cannot_do() {
    let jpeg = &MozjpegEncoder;
    let src = test_image(ColorType::Rgb);

    assert!(matches!(
        jpeg.encode(&src, &Target::Lossless.into_params()),
        Err(Error::Unsupported {
            format: Format::Jpeg,
            ..
        })
    ));
    assert!(matches!(
        jpeg.encode(&src, &EncodeParams::default()),
        Err(Error::InvalidParams(_))
    ));
    let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
    assert!(matches!(
        jpeg.encode(&hdr, &quality(75.0)),
        Err(Error::Unsupported {
            format: Format::Jpeg,
            ..
        })
    ));
    let bad_opt = quality(75.0).with_codec_opt("jpeg", "turbo", "yes");
    assert!(matches!(
        jpeg.encode(&src, &bad_opt),
        Err(Error::InvalidParams(_))
    ));
}

#[test]
fn jpeg_codec_options_take_effect() {
    let reg = registry();
    let jpeg = &MozjpegEncoder;
    let src = test_image(ColorType::Rgb);
    let progressive = jpeg
        .encode(
            &src,
            &quality(75.0).with_codec_opt("jpeg", "progressive", "true"),
        )
        .unwrap();
    let baseline = jpeg
        .encode(
            &src,
            &quality(75.0).with_codec_opt("jpeg", "progressive", "false"),
        )
        .unwrap();
    // SOF2 marks a progressive frame, SOF0 a baseline one.
    assert!(progressive.windows(2).any(|w| w == [0xFF, 0xC2]));
    assert!(baseline.windows(2).any(|w| w == [0xFF, 0xC0]));
    assert!(!baseline.windows(2).any(|w| w == [0xFF, 0xC2]));
    // Both decode to the same picture.
    let a = reg
        .decode(&progressive, &DecodeOpts::default())
        .unwrap()
        .image;
    let b = reg.decode(&baseline, &DecodeOpts::default()).unwrap().image;
    assert_close(&a, &b, 2.0, "progressive vs baseline");
}

#[test]
fn jpeg_icc_survives_a_round_trip() {
    let reg = registry();
    let src = test_image(ColorType::Rgb).with_icc(Some(b"fake profile bytes".to_vec()));
    let bytes = MozjpegEncoder.encode(&src, &quality(80.0)).unwrap();
    let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
    assert_eq!(back.icc(), src.icc());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn png_output_is_smaller_than_the_plain_writer() {
    use sqzer_codecs::png::PngEncoder;
    use sqzer_core::codec::Encoder;
    let reg = registry();
    let src = test_image(ColorType::Rgba);
    let params = Target::Lossless.into_params();
    let plain = PngEncoder.encode(&src, &params).unwrap().len();
    let optimised = reg
        .encoder(Format::Png)
        .unwrap()
        .encode(&src, &params)
        .unwrap()
        .len();
    assert!(
        optimised < plain,
        "oxipng {optimised} bytes should beat the plain writer's {plain}"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn png_codec_options_take_effect() {
    let reg = registry();
    let png = reg.encoder(Format::Png).unwrap();
    let src = test_image(ColorType::Rgb);
    let interlaced = png
        .encode(
            &src,
            &quality(50.0).with_codec_opt("png", "interlace", "true"),
        )
        .unwrap();
    // IHDR is the first chunk: 8 signature + 8 header + 13 data, of which
    // the interlace method is the last byte.
    assert_eq!(interlaced[28], 1, "interlace flag");
    assert_eq!(
        reg.decode(&interlaced, &DecodeOpts::default())
            .unwrap()
            .image,
        src
    );
    assert!(matches!(
        png.encode(&src, &quality(50.0).with_codec_opt("png", "brute", "yes")),
        Err(Error::InvalidParams(_))
    ));
}

/// The pattern with gray replicated into three channels, which is what a
/// container without a gray layout hands back.
#[cfg(any(
    feature = "webp-lossless",
    all(feature = "avif", not(target_arch = "wasm32"))
))]
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

// ------------------------------------------------------------------ WebP

#[cfg(feature = "webp-lossless")]
mod webp {
    use super::*;
    // The portable writer itself: a native build's registry hands out
    // `webpx` for this format.
    use sqzer_codecs::webp::WebPLosslessEncoder;
    use sqzer_core::codec::Encoder;

    #[test]
    fn round_trips_rgb_and_rgba_exactly() {
        let reg = registry();
        let webp = &WebPLosslessEncoder;
        for color in [ColorType::Rgb, ColorType::Rgba] {
            let src = test_image(color);
            let bytes = webp.encode(&src, &Target::Lossless.into_params()).unwrap();
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WEBP");
            let decoded = reg.decode(&bytes, &DecodeOpts::default()).unwrap();
            assert_eq!(decoded.info.format, Format::WebP);
            assert!(!decoded.info.animated);
            assert_eq!(decoded.image, src, "layout {color:?}");
        }
    }

    #[test]
    fn gray_comes_back_as_rgb() {
        let reg = registry();
        let webp = &WebPLosslessEncoder;
        for color in [ColorType::Gray, ColorType::GrayAlpha] {
            let bytes = webp
                .encode(&test_image(color), &Target::Lossless.into_params())
                .unwrap();
            let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
            assert_eq!(back, gray_as_rgb(color), "{color:?}");
        }
    }

    #[test]
    fn sixteen_bit_is_rounded_to_eight() {
        let reg = registry();
        let src = test_image_u16(ColorType::Rgba);
        let bytes = WebPLosslessEncoder.encode(&src, &quality(60.0)).unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back, test_image(ColorType::Rgba));
    }

    #[test]
    fn icc_survives_a_round_trip() {
        let reg = registry();
        let src = test_image(ColorType::Rgb).with_icc(Some(b"fake profile bytes".to_vec()));
        let bytes = WebPLosslessEncoder
            .encode(&src, &Target::Lossless.into_params())
            .unwrap();
        let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
        assert_eq!(back.icc(), src.icc());
        assert_eq!(back.samples(), src.samples());
    }

    #[test]
    fn predictor_option_takes_effect() {
        let reg = registry();
        let webp = &WebPLosslessEncoder;
        let src = test_image(ColorType::Rgb);
        let with = webp
            .encode(
                &src,
                &quality(50.0).with_codec_opt("webp", "predictor", "true"),
            )
            .unwrap();
        let without = webp
            .encode(
                &src,
                &quality(50.0).with_codec_opt("webp", "predictor", "false"),
            )
            .unwrap();
        assert_ne!(with, without, "the option changed nothing");
        for bytes in [with, without] {
            assert_eq!(
                reg.decode(&bytes, &DecodeOpts::default()).unwrap().image,
                src
            );
        }
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let webp = &WebPLosslessEncoder;
        let src = test_image(ColorType::Rgb);
        assert!(matches!(
            webp.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(matches!(
            webp.encode(&hdr, &quality(75.0)),
            Err(Error::Unsupported {
                format: Format::WebP,
                ..
            })
        ));
        assert!(matches!(
            webp.encode(&src, &quality(75.0).with_codec_opt("webp", "lossy", "true")),
            Err(Error::InvalidParams(_))
        ));
        assert!(matches!(
            webp.encode(
                &src,
                &quality(75.0).with_codec_opt("webp", "predictor", "maybe")
            ),
            Err(Error::InvalidParams(_))
        ));
    }
}

// ------------------------------------------------------------------ AVIF

#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
mod avif {
    use super::*;
    // The portable writer itself: a native build's registry hands out
    // `libavif` for this format.
    use common::mae_channel;
    use sqzer_codecs::avif::RavifEncoder;
    use sqzer_core::codec::Encoder;
    use sqzer_core::image::SampleFormat;
    use sqzer_core::params::Subsampling;

    /// Decode and bring the result back to 8 bits, since the payload is
    /// 10-bit by default.
    fn decode_u8(reg: &sqzer_core::Registry, bytes: &[u8]) -> Image {
        let decoded = reg.decode(bytes, &DecodeOpts::default()).unwrap();
        assert_eq!(decoded.info.format, Format::Avif);
        assert!(!decoded.info.animated);
        decoded.image.to_u8(Format::Avif).unwrap().into_owned()
    }

    #[test]
    fn round_trips_close_to_the_source() {
        let reg = registry();
        let src = test_image(ColorType::Rgb);
        let bytes = RavifEncoder.encode(&src, &quality(90.0)).unwrap();
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
        let bytes = RavifEncoder.encode(&src, &quality(90.0)).unwrap();
        let back = decode_u8(&reg, &bytes);
        assert_eq!(back.color(), ColorType::Rgba);
        let alpha_err = mae_channel(&back, &src, 3);
        assert!(alpha_err < 2.0, "alpha mean absolute error {alpha_err:.2}");
        assert_close(&back, &src, 4.0, "q90 rgba round trip");
    }

    #[test]
    fn gray_encodes_as_rgb() {
        let reg = registry();
        let avif = &RavifEncoder;
        for color in [ColorType::Gray, ColorType::GrayAlpha] {
            let bytes = avif.encode(&test_image(color), &quality(90.0)).unwrap();
            let back = decode_u8(&reg, &bytes);
            assert_close(&back, &gray_as_rgb(color), 4.0, &format!("{color:?}"));
        }
    }

    #[test]
    fn sixteen_bit_is_accepted_by_conversion() {
        let reg = registry();
        let bytes = RavifEncoder
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
        let src = test_image(ColorType::Rgb);
        let avif = &RavifEncoder;
        let low = avif.encode(&src, &quality(30.0)).unwrap().len();
        let high = avif.encode(&src, &quality(95.0)).unwrap().len();
        assert!(
            low < high,
            "q30 {low} bytes should be smaller than q95 {high}"
        );
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
            let bytes = RavifEncoder.encode(&src, &params).unwrap();
            assert_close(
                &decode_u8(&reg, &bytes),
                &src,
                8.0,
                &format!("effort {effort}"),
            );
        }
    }

    #[test]
    fn bit_depth_option_controls_the_payload() {
        let reg = registry();
        let avif = &RavifEncoder;
        let src = test_image(ColorType::Rgb);
        let ten = avif.encode(&src, &quality(80.0)).unwrap();
        let eight = avif
            .encode(
                &src,
                &quality(80.0).with_codec_opt("avif", "bit_depth", "8"),
            )
            .unwrap();
        let ten = reg.decode(&ten, &DecodeOpts::default()).unwrap().image;
        let eight = reg.decode(&eight, &DecodeOpts::default()).unwrap().image;
        assert_eq!(ten.sample_format(), SampleFormat::U16);
        assert_eq!(eight.sample_format(), SampleFormat::U8);
        assert_close(&eight, &src, 4.0, "8-bit payload");
    }

    #[test]
    fn color_model_and_alpha_quality_options_take_effect() {
        let reg = registry();
        let avif = &RavifEncoder;
        let src = test_image(ColorType::Rgba);
        let ycbcr = avif.encode(&src, &quality(80.0)).unwrap();
        let rgb = avif
            .encode(
                &src,
                &quality(80.0).with_codec_opt("avif", "color_model", "rgb"),
            )
            .unwrap();
        assert_ne!(ycbcr, rgb);
        assert_close(&decode_u8(&reg, &rgb), &src, 4.0, "rgb model");

        let rough_alpha = avif
            .encode(
                &src,
                &quality(80.0).with_codec_opt("avif", "alpha_quality", "5"),
            )
            .unwrap();
        assert!(
            rough_alpha.len() < ycbcr.len(),
            "alpha quality changed nothing"
        );
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        let avif = &RavifEncoder;
        let src = test_image(ColorType::Rgb);
        let unsupported = |r: Result<Vec<u8>, Error>| {
            matches!(
                r,
                Err(Error::Unsupported {
                    format: Format::Avif,
                    ..
                })
            )
        };
        assert!(unsupported(
            avif.encode(&src, &Target::Lossless.into_params())
        ));
        assert!(unsupported(avif.encode(
            &src,
            &EncodeParams {
                subsampling: Subsampling::S420,
                ..quality(75.0)
            }
        )));
        assert!(unsupported(avif.encode(
            &src.clone().with_icc(Some(b"fake profile bytes".to_vec())),
            &quality(75.0)
        )));
        let hdr = Image::new(1, 1, ColorType::Rgb, Samples::F32(vec![0.5; 3])).unwrap();
        assert!(unsupported(avif.encode(&hdr, &quality(75.0))));

        assert!(matches!(
            avif.encode(&src, &EncodeParams::default()),
            Err(Error::InvalidParams(_))
        ));
        for (key, value) in [
            ("tune", "ssim"),
            ("alpha_quality", "0"),
            ("bit_depth", "12"),
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
