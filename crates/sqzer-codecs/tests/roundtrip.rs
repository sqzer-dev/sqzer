//! End-to-end checks over the encoders: PNG in, PNG or JPEG out, decoded
//! back through the registry.

#![cfg(all(feature = "png", feature = "jpeg"))]
// Synthetic pixel data: the truncating casts are the point.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{H, IntoParams, W, assert_close, quality, test_image};
use sqzer_codecs::registry;
use sqzer_core::Error;
use sqzer_core::codec::{Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{DecodeOpts, EncodeParams, Target};

#[test]
fn feature_registry_lists_the_portable_backends() {
    let reg = registry();
    let enc: Vec<_> = reg.encoders().map(|e| e.caps().format).collect();
    assert_eq!(enc, vec![Format::Jpeg, Format::Png]);
    assert!(reg.encoders().all(|e| e.caps().tier == Tier::Portable));
    assert!(reg.decoders().all(|d| d.caps().tier == Tier::Portable));

    let dec: Vec<_> = reg.decoders().map(|d| d.caps().format).collect();
    let mut expected = vec![Format::Jpeg, Format::Png];
    if cfg!(feature = "webp-lossless") {
        expected.push(Format::WebP);
    }
    if cfg!(all(feature = "avif", not(target_arch = "wasm32"))) {
        expected.push(Format::Avif);
    }
    if cfg!(feature = "jxl-decode") {
        expected.push(Format::Jxl);
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
    let jpeg = reg.encoder(Format::Jpeg).unwrap();

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
fn jpeg_accepts_alpha_and_16_bit_by_conversion() {
    let reg = registry();
    let jpeg = reg.encoder(Format::Jpeg).unwrap();
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
    let reg = registry();
    let jpeg = reg.encoder(Format::Jpeg).unwrap();
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
    let jpeg = reg.encoder(Format::Jpeg).unwrap();
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
    let bytes = reg
        .encoder(Format::Jpeg)
        .unwrap()
        .encode(&src, &quality(80.0))
        .unwrap();
    let back = reg.decode(&bytes, &DecodeOpts::default()).unwrap().image;
    assert_eq!(back.icc(), src.icc());
}
