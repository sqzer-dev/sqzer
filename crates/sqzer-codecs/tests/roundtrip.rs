//! End-to-end checks over the first backend pair: PNG in, PNG or JPEG out.

#![cfg(all(feature = "png", feature = "jpeg"))]
// Synthetic pixel data: the truncating casts are the point.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use sqzer_codecs::registry;
use sqzer_core::Error;
use sqzer_core::codec::{Format, Tier};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::{DecodeOpts, EncodeParams, Target};

const W: u32 = 48;
const H: u32 = 32;

/// A smooth gradient with a hard edge, so subsampling and DCT both matter.
fn test_image(color: ColorType) -> Image {
    let ch = color.channels();
    let mut samples = Vec::with_capacity(W as usize * H as usize * ch);
    for y in 0..H {
        for x in 0..W {
            let r = (x * 255 / (W - 1)) as u8;
            let g = (y * 255 / (H - 1)) as u8;
            let b = if x < W / 2 { 40 } else { 220 };
            let a = if y % 2 == 0 { 255 } else { 128 };
            let gray = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
            match color {
                ColorType::Gray => samples.push(gray),
                ColorType::GrayAlpha => samples.extend([gray, a]),
                ColorType::Rgb => samples.extend([r, g, b]),
                ColorType::Rgba => samples.extend([r, g, b, a]),
            }
        }
    }
    Image::from_u8(W, H, color, samples).unwrap()
}

fn quality(q: f32) -> EncodeParams {
    EncodeParams {
        target: Target::Quality(q),
        ..Default::default()
    }
}

#[test]
fn feature_registry_has_the_first_pair() {
    let reg = registry();
    let enc: Vec<_> = reg.encoders().map(|e| e.caps().format).collect();
    assert_eq!(enc, vec![Format::Png, Format::Jpeg]);
    assert!(reg.encoders().all(|e| e.caps().tier == Tier::Portable));
    assert_eq!(
        reg.decoders().map(|d| d.caps().format).collect::<Vec<_>>(),
        vec![Format::Png]
    );
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
fn jpeg_output_decodes_close_to_the_source() {
    let reg = registry();
    let src = test_image(ColorType::Rgb);
    let jpeg = reg.encoder(Format::Jpeg).unwrap();

    let bytes = jpeg.encode(&src, &quality(90.0)).unwrap();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9]);

    let mut dec = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes));
    let pixels = dec.decode().unwrap();
    let info = dec.info().unwrap();
    assert_eq!((u32::from(info.width), u32::from(info.height)), (W, H));
    assert_eq!(pixels.len(), src.samples().len());

    let src_px = src.samples().as_u8().unwrap();
    let mae = pixels
        .iter()
        .zip(src_px)
        .map(|(&a, &b)| f64::from(a.abs_diff(b)))
        .sum::<f64>()
        / pixels.len() as f64;
    assert!(mae < 4.0, "mean absolute error {mae} too high for q90");
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
        let mut dec = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes));
        dec.decode().unwrap();
        let info = dec.info().unwrap();
        assert_eq!((u32::from(info.width), u32::from(info.height)), (W, H));
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
}

trait IntoParams {
    fn into_params(self) -> EncodeParams;
}

impl IntoParams for Target {
    fn into_params(self) -> EncodeParams {
        EncodeParams {
            target: self,
            ..Default::default()
        }
    }
}
