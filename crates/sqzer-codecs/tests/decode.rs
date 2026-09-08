//! Decoder checks over the committed fixtures. Every file under
//! `tests/fixtures` encodes the same synthetic pattern (see `common`), so
//! lossless formats are compared exactly and lossy ones within a bound.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{H, W, assert_close, fixture, is_icc, mae_channel, test_image, test_image_u16};
use sqzer_codecs::registry;
use sqzer_core::codec::Format;
use sqzer_core::image::{ColorType, Image, SampleFormat};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Registry};

fn decode(reg: &Registry, name: &str) -> (Image, sqzer_core::codec::FormatInfo) {
    let out = reg
        .decode(&fixture(name), &DecodeOpts::default())
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    (out.image, out.info)
}

/// `Decoder::dimensions` reads the header only, so it must agree with the
/// decoded picture on every fixture, and say nothing about other formats.
#[test]
fn header_dimensions_match_the_decode() {
    let reg = registry();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "md") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let Some((_, decoder)) = reg.probe(&bytes) else {
            continue;
        };
        assert!(!decoder.caps().name.is_empty());
        let dims = decoder.dimensions(&bytes);
        let img = decoder.decode(&bytes, &DecodeOpts::default()).unwrap();
        // Stored dimensions; a rotated fixture comes out with the axes
        // swapped once orientation is applied.
        let stored = if path.to_string_lossy().contains("rot90") {
            (img.height(), img.width())
        } else {
            (img.width(), img.height())
        };
        assert_eq!(dims, Some(stored), "{}", path.display());
        for other in reg.decoders() {
            if other.caps().format != decoder.caps().format {
                assert_eq!(other.dimensions(&bytes), None, "{}", path.display());
            }
        }
        checked += 1;
    }
    assert!(checked > 5, "only {checked} fixtures were decodable");
}

fn assert_too_large(reg: &Registry, name: &str) {
    let opts = DecodeOpts {
        max_pixels: u64::from(W * H) - 1,
        ..Default::default()
    };
    assert!(
        matches!(
            reg.decode(&fixture(name), &opts),
            Err(Error::TooLarge { pixels, .. }) if pixels == u64::from(W * H)
        ),
        "{name}: pixel limit not enforced"
    );
}

// ------------------------------------------------------------------ JPEG

#[cfg(feature = "jpeg")]
mod jpeg {
    use super::*;

    #[test]
    fn rgb_420_progressive() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.jpg");
        assert_eq!(info.format, Format::Jpeg);
        assert!(!info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_eq!(img.icc(), None);
        assert_close(&img, &test_image(ColorType::Rgb), 5.0, "pattern-rgb.jpg");
    }

    #[test]
    fn grayscale_stays_gray() {
        let (img, _) = decode(&registry(), "pattern-gray.jpg");
        assert_eq!(img.color(), ColorType::Gray);
        assert_close(&img, &test_image(ColorType::Gray), 3.0, "pattern-gray.jpg");
    }

    #[test]
    fn icc_is_kept() {
        let (img, _) = decode(&registry(), "pattern-icc.jpg");
        assert!(img.icc().is_some_and(is_icc), "ICC profile missing");
        assert_close(&img, &test_image(ColorType::Rgb), 3.0, "pattern-icc.jpg");
    }

    #[test]
    fn exif_orientation_is_applied_unless_disabled() {
        let reg = registry();
        let (img, _) = decode(&reg, "pattern-rot90.jpg");
        assert_eq!((img.width(), img.height()), (W, H));
        assert_close(&img, &test_image(ColorType::Rgb), 3.0, "pattern-rot90.jpg");

        let raw = reg
            .decode(
                &fixture("pattern-rot90.jpg"),
                &DecodeOpts {
                    apply_orientation: false,
                    ..Default::default()
                },
            )
            .unwrap()
            .image;
        assert_eq!((raw.width(), raw.height()), (H, W));
    }

    #[test]
    fn pixel_limit() {
        assert_too_large(&registry(), "pattern-rgb.jpg");
    }

    #[test]
    fn corrupt_file_is_a_codec_error() {
        // A truncated JPEG decodes leniently, as in a browser; a broken
        // header does not.
        let err = registry()
            .decode(
                b"\xFF\xD8\xFF\xE0 nothing like a jfif segment",
                &DecodeOpts::default(),
            )
            .unwrap_err();
        assert!(matches!(err, Error::Codec(_)), "{err}");
    }
}

// ------------------------------------------------------------------ WebP

#[cfg(feature = "webp-lossless")]
mod webp {
    use super::*;

    #[test]
    fn lossless_is_exact() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.webp");
        assert_eq!(info.format, Format::WebP);
        assert!(!info.animated);
        assert_eq!(img, test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.webp");
        assert_eq!(img, test_image(ColorType::Rgba));
    }

    #[test]
    fn lossy_is_close() {
        let reg = registry();
        let (img, _) = decode(&reg, "pattern-lossy.webp");
        assert_close(&img, &test_image(ColorType::Rgb), 5.0, "pattern-lossy.webp");

        let (img, _) = decode(&reg, "pattern-lossy-alpha.webp");
        let expected = test_image(ColorType::Rgba);
        assert_eq!(img.color(), ColorType::Rgba);
        assert_close(&img, &expected, 5.0, "pattern-lossy-alpha.webp");
        // libwebp stores alpha losslessly by default.
        assert!(mae_channel(&img, &expected, 3) < 1e-9, "alpha plane");
    }

    #[test]
    fn animation_is_detected_and_first_frame_returned() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-anim.webp");
        assert!(info.animated);
        assert_eq!(img, test_image(ColorType::Rgba));
    }

    #[test]
    fn exif_orientation_is_applied() {
        let (img, _) = decode(&registry(), "pattern-rot90.webp");
        assert_eq!(img, test_image(ColorType::Rgb));
    }

    #[test]
    fn icc_is_kept() {
        let (img, _) = decode(&registry(), "pattern-icc.webp");
        assert!(img.icc().is_some_and(is_icc), "ICC profile missing");
        assert_eq!(img.samples(), test_image(ColorType::Rgb).samples());
    }

    #[test]
    fn pixel_limit() {
        assert_too_large(&registry(), "pattern-rgb.webp");
    }
}

// ------------------------------------------------------------------ JXL

#[cfg(feature = "jxl-decode")]
mod jxl {
    use super::*;

    #[test]
    fn lossless_is_exact() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.jxl");
        assert_eq!(info.format, Format::Jxl);
        assert!(!info.animated);
        assert_eq!(img, test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.jxl");
        assert_eq!(img, test_image(ColorType::Rgba));
        let (img, _) = decode(&reg, "pattern-gray.jxl");
        assert_eq!(img, test_image(ColorType::Gray));
    }

    #[test]
    fn sixteen_bit_stays_sixteen_bit() {
        let (img, _) = decode(&registry(), "pattern-rgb16.jxl");
        assert_eq!(img.sample_format(), SampleFormat::U16);
        assert_eq!(img, test_image_u16(ColorType::Rgb));
    }

    #[test]
    fn lossy_xyb_renders_to_srgb() {
        let (img, _) = decode(&registry(), "pattern-lossy.jxl");
        assert_eq!(img.icc(), None);
        assert_close(&img, &test_image(ColorType::Rgb), 4.0, "pattern-lossy.jxl");
    }

    #[test]
    fn icc_is_kept_and_samples_untouched() {
        let (img, _) = decode(&registry(), "pattern-icc.jxl");
        assert!(img.icc().is_some_and(is_icc), "ICC profile missing");
        assert_eq!(img.samples(), test_image(ColorType::Rgb).samples());
    }

    #[test]
    fn container_format_is_recognised() {
        let bytes = fixture("pattern-container.jxl");
        assert_eq!(
            &bytes[4..8],
            b"JXL ",
            "fixture is not in the container format"
        );
        let (img, info) = decode(&registry(), "pattern-container.jxl");
        assert_eq!(info.format, Format::Jxl);
        assert_eq!(img, test_image(ColorType::Rgb));
    }

    #[test]
    fn pixel_limit() {
        assert_too_large(&registry(), "pattern-rgb.jxl");
    }
}

// ------------------------------------------------------------------ AVIF

#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
mod avif {
    use super::*;

    #[test]
    fn rgb_420_limited_range() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.avif");
        assert_eq!(info.format, Format::Avif);
        assert!(!info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_close(&img, &test_image(ColorType::Rgb), 6.0, "pattern-rgb.avif");
    }

    #[test]
    fn ten_bit_decodes_to_u16() {
        let (img, _) = decode(&registry(), "pattern-10bit.avif");
        assert_eq!(img.sample_format(), SampleFormat::U16);
        assert_close(
            &img,
            &test_image_u16(ColorType::Rgb),
            6.0 * 257.0,
            "pattern-10bit.avif",
        );
    }

    #[test]
    fn monochrome_decodes_to_gray() {
        let (img, _) = decode(&registry(), "pattern-gray.avif");
        assert_eq!(img.color(), ColorType::Gray);
        assert_close(&img, &test_image(ColorType::Gray), 4.0, "pattern-gray.avif");
    }

    #[test]
    fn alpha_item_becomes_a_channel() {
        // ravif writes 10-bit by default, so this one comes back as u16.
        let (img, _) = decode(&registry(), "pattern-rgba.avif");
        assert_eq!(img.color(), ColorType::Rgba);
        assert_eq!(img.sample_format(), SampleFormat::U16);
        let expected = test_image_u16(ColorType::Rgba);
        assert_close(&img, &expected, 5.0 * 257.0, "pattern-rgba.avif");
        assert!(mae_channel(&img, &expected, 3) < 3.0 * 257.0, "alpha plane");
    }

    #[test]
    fn pixel_limit() {
        assert_too_large(&registry(), "pattern-rgb.avif");
    }
}

// ----------------------------------------------------------- cross-format

#[cfg(all(feature = "jpeg", feature = "webp-lossless", feature = "jxl-decode"))]
#[test]
fn the_same_icc_comes_back_from_every_container() {
    let reg = registry();
    let jpg = decode(&reg, "pattern-icc.jpg").0.icc().unwrap().to_vec();
    let webp = decode(&reg, "pattern-icc.webp").0.icc().unwrap().to_vec();
    let jxl = decode(&reg, "pattern-icc.jxl").0.icc().unwrap().to_vec();
    assert_eq!(jpg, webp);
    assert_eq!(jpg, jxl);
}
