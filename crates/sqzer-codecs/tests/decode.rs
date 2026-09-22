//! Decoder checks over the committed fixtures. Every file under
//! `tests/fixtures` encodes the same synthetic pattern (see `common`), so
//! lossless formats are compared exactly and lossy ones within a bound.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{
    H, W, assert_close, assert_pixels, contains, exif_orientation, fixture, is_icc, mae_channel,
    test_image, test_image_u16,
};
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
        // swapped once orientation is applied. HEIF is an exception: its
        // header reports the displayed size, after the container's own
        // rotation, so header and decode agree as they are.
        let name = path.to_string_lossy();
        // JPEG XL is another: `jxl-oxide` reports the oriented size too.
        let rotated = name.contains("rot90") || name.contains("meta");
        let stored = if rotated && !name.ends_with(".heic") && !name.ends_with(".jxl") {
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
    fn exif_and_xmp_ride_on_the_image_with_the_orientation_reset() {
        let reg = registry();
        let (img, _) = decode(&reg, "pattern-meta.jpg");
        assert_eq!((img.width(), img.height()), (W, H));
        let exif = img.exif().expect("EXIF kept");
        assert!(!exif.starts_with(b"Exif"), "prefix stripped");
        assert_eq!(exif_orientation(exif), Some(1), "tag reset once applied");
        assert!(contains(exif, b"sqzer"), "Artist survives");
        assert!(contains(img.xmp().expect("XMP kept"), b"test pattern"));
        // Orientation not applied: the tag says what the pixels need.
        let raw = reg
            .decode(
                &fixture("pattern-meta.jpg"),
                &DecodeOpts {
                    apply_orientation: false,
                    ..Default::default()
                },
            )
            .unwrap()
            .image;
        assert_eq!((raw.width(), raw.height()), (H, W));
        assert_eq!(exif_orientation(raw.exif().unwrap()), Some(6));
        // An untagged file carries nothing.
        let (plain, _) = decode(&reg, "pattern-rgb.jpg");
        assert_eq!((plain.exif(), plain.xmp()), (None, None));
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
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.webp");
        assert_pixels(&img, &test_image(ColorType::Rgba));
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
        assert_pixels(&img, &test_image(ColorType::Rgba));
    }

    #[test]
    fn exif_orientation_is_applied() {
        let (img, _) = decode(&registry(), "pattern-rot90.webp");
        assert_pixels(&img, &test_image(ColorType::Rgb));
    }

    #[test]
    fn exif_and_xmp_ride_on_the_image_with_the_orientation_reset() {
        let reg = registry();
        let (img, _) = decode(&reg, "pattern-meta.webp");
        assert_pixels(&img, &test_image(ColorType::Rgb));
        assert_eq!(exif_orientation(img.exif().unwrap()), Some(1));
        assert!(contains(img.exif().unwrap(), b"sqzer"));
        assert!(contains(img.xmp().unwrap(), b"test pattern"));
        let raw = reg
            .decode(
                &fixture("pattern-meta.webp"),
                &DecodeOpts {
                    apply_orientation: false,
                    ..Default::default()
                },
            )
            .unwrap()
            .image;
        assert_eq!(exif_orientation(raw.exif().unwrap()), Some(6));
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
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.jxl");
        assert_pixels(&img, &test_image(ColorType::Rgba));
        let (img, _) = decode(&reg, "pattern-gray.jxl");
        assert_pixels(&img, &test_image(ColorType::Gray));
    }

    #[test]
    fn sixteen_bit_stays_sixteen_bit() {
        let (img, _) = decode(&registry(), "pattern-rgb16.jxl");
        assert_eq!(img.sample_format(), SampleFormat::U16);
        assert_pixels(&img, &test_image_u16(ColorType::Rgb));
    }

    #[test]
    fn lossy_xyb_renders_to_srgb() {
        let (img, _) = decode(&registry(), "pattern-lossy.jxl");
        assert_eq!(img.icc(), None);
        assert_close(&img, &test_image(ColorType::Rgb), 4.0, "pattern-lossy.jxl");
    }

    /// Made by `cjxl` from `pattern-rot90.jpg`: the codestream carries the
    /// orientation, which `jxl-oxide` applies, and the Exif box repeats it,
    /// which the decoder resets so an output cannot rotate twice.
    #[test]
    fn container_exif_and_xmp_ride_on_the_image() {
        let (img, _) = decode(&registry(), "pattern-meta.jxl");
        assert_eq!((img.width(), img.height()), (W, H));
        assert_close(&img, &test_image(ColorType::Rgb), 3.0, "pattern-meta.jxl");
        assert_eq!(exif_orientation(img.exif().unwrap()), Some(1));
        assert!(contains(img.exif().unwrap(), b"sqzer"));
        assert!(contains(img.xmp().unwrap(), b"test pattern"));
        // A bare codestream has no boxes to carry any.
        let (plain, _) = decode(&registry(), "pattern-rgb.jxl");
        assert_eq!((plain.exif(), plain.xmp()), (None, None));
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
        assert_pixels(&img, &test_image(ColorType::Rgb));
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

// ------------------------------------------------------------------ HEIC

/// Every build recognises HEIC; only `native-heif` reads it, and on musl
/// not even that, since a static binary cannot load `libheif` and there
/// is no OS decoder. The error names the feature instead of calling the
/// file unrecognised.
#[cfg(all(
    feature = "heif",
    any(not(feature = "native-heif"), target_env = "musl")
))]
#[test]
fn heic_is_recognised_but_needs_the_feature() {
    let reg = registry();
    let bytes = fixture("pattern-rgb.heic");
    assert!(reg.probe(&bytes).is_none());
    assert_eq!(reg.identify(&bytes).map(|i| i.format), Some(Format::Heic));
    assert!(!reg.has_decoder(Format::Heic));
    let err = reg.decode(&bytes, &DecodeOpts::default()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::DecoderUnavailable {
                format: Format::Heic,
                available_in: &["native-heif"],
                reason: None,
            }
        ),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "no decoder for HEIC in this build (enable one of: native-heif)"
    );
}

/// The conformance suite of ADR-0005 D5: every HEIC decoder this build has
/// is run over every fixture and must produce the same picture within the
/// same tolerances. A backend that cannot run on this machine has to say
/// so cleanly; on Windows that is allowed to be the whole story, since
/// GitHub's runners are Windows Server without the Store codecs and
/// `libheif` is nowhere on `PATH`. Everywhere else every compiled-in
/// backend must work: CI installs `libheif`, and every macOS has `ImageIO`.
#[cfg(all(feature = "native-heif", not(target_env = "musl")))]
mod heic {
    use super::*;
    use sqzer_core::codec::Decoder;
    use sqzer_core::image::Orientation;

    /// Run `check` on every HEIC backend that can run here.
    fn for_each_backend(check: impl Fn(&dyn Decoder)) {
        let reg = registry();
        let backends: Vec<&dyn Decoder> = reg
            .decoders()
            .filter(|d| d.caps().format == Format::Heic)
            .collect();
        assert!(!backends.is_empty(), "native-heif registered no decoder");
        for d in backends {
            let name = d.caps().name;
            match d.available() {
                Ok(()) => check(d),
                Err(reason) => {
                    // Unavailable is a first-class answer, never a crash
                    // and never a wrong image.
                    let err = d
                        .decode(&fixture("pattern-rgb.heic"), &DecodeOpts::default())
                        .unwrap_err();
                    assert!(
                        matches!(
                            &err,
                            Error::DecoderUnavailable {
                                format: Format::Heic,
                                reason: Some(r),
                                ..
                            } if r.starts_with(name)
                        ),
                        "{name}: {err}"
                    );
                    let windows = cfg!(windows);
                    assert!(windows, "{name} must be usable on this platform: {reason}");
                    eprintln!("{name}: unavailable here, decode checks skipped: {reason}");
                }
            }
        }
    }

    fn decode_with(d: &dyn Decoder, name: &str) -> Image {
        d.decode(&fixture(name), &DecodeOpts::default())
            .unwrap_or_else(|e| panic!("{}: {name}: {e}", d.caps().name))
    }

    /// The gray pattern as a backend may hand it back: one channel, or
    /// three equal ones.
    fn assert_gray(img: &Image, limit: f64, what: &str) {
        match img.color() {
            ColorType::Gray => assert_close(img, &test_image(ColorType::Gray), limit, what),
            ColorType::Rgb => {
                let rgb = img.samples().as_u8().expect("8-bit");
                for c in 1..3 {
                    let spread = rgb
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|px| f64::from(px[0].abs_diff(px[c])))
                        .sum::<f64>()
                        / f64::from(W * H);
                    assert!(
                        spread < 2.0,
                        "{what}: channel {c} differs from R by {spread:.2}"
                    );
                }
                let gray: Vec<u8> = rgb.iter().step_by(3).copied().collect();
                let gray =
                    Image::from_u8(img.width(), img.height(), ColorType::Gray, gray).unwrap();
                assert_close(&gray, &test_image(ColorType::Gray), limit, what);
            }
            other => panic!("{what}: unexpected layout {other:?}"),
        }
    }

    #[test]
    fn rgb_420() {
        for_each_backend(|d| {
            let img = decode_with(d, "pattern-rgb.heic");
            assert_eq!(img.color(), ColorType::Rgb, "{}", d.caps().name);
            assert_eq!(img.icc(), None, "{}", d.caps().name);
            assert_close(&img, &test_image(ColorType::Rgb), 6.0, d.caps().name);
        });
        // And through the registry, whichever backend it picks.
        let reg = registry();
        if reg.has_decoder(Format::Heic) {
            let (img, info) = decode(&reg, "pattern-rgb.heic");
            assert_eq!(info.format, Format::Heic);
            assert!(!info.animated);
            assert_eq!(img.color(), ColorType::Rgb);
        }
    }

    #[test]
    fn alpha_plane_becomes_a_channel() {
        for_each_backend(|d| {
            let img = decode_with(d, "pattern-rgba.heic");
            assert_eq!(img.color(), ColorType::Rgba, "{}", d.caps().name);
            let expected = test_image(ColorType::Rgba);
            assert_close(&img, &expected, 6.0, d.caps().name);
            assert!(
                mae_channel(&img, &expected, 3) < 6.0,
                "{}: alpha plane",
                d.caps().name
            );
        });
    }

    #[test]
    fn monochrome_decodes_to_gray() {
        for_each_backend(|d| {
            let img = decode_with(d, "pattern-gray.heic");
            assert_gray(&img, 4.0, d.caps().name);
        });
    }

    #[test]
    fn icc_is_kept() {
        for_each_backend(|d| {
            let img = decode_with(d, "pattern-icc.heic");
            assert!(
                img.icc().is_some_and(is_icc),
                "{}: ICC missing",
                d.caps().name
            );
            assert_close(&img, &test_image(ColorType::Rgb), 6.0, d.caps().name);
        });
    }

    #[test]
    fn container_rotation_is_always_applied() {
        for_each_backend(|d| {
            let name = d.caps().name;
            assert_eq!(
                d.dimensions(&fixture("pattern-rot90.heic")),
                Some((W, H)),
                "{name}"
            );
            let img = decode_with(d, "pattern-rot90.heic");
            assert_eq!((img.width(), img.height()), (W, H), "{name}");
            // 4:2:0 chroma was subsampled along the stored axes, so the
            // hard edge blurs a little more than in the upright file.
            assert_close(&img, &test_image(ColorType::Rgb), 8.0, name);

            // `irot` is container geometry, like the JPEG XL orientation
            // field, not Exif metadata: the flag does not turn it off.
            let raw = d
                .decode(
                    &fixture("pattern-rot90.heic"),
                    &DecodeOpts {
                        apply_orientation: false,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(raw, img, "{name}");
        });
        // The value every backend applies comes from the container walk.
        let header = sqzer_codecs::heif::header(&fixture("pattern-rot90.heic")).unwrap();
        assert_eq!(header.orientation, Orientation::Rotate90);
    }

    #[test]
    fn ten_bit_decodes_to_the_pattern() {
        for_each_backend(|d| {
            let name = d.caps().name;
            let img = decode_with(d, "pattern-10bit.heic");
            assert_eq!(img.color(), ColorType::Rgb, "{name}");
            match img.sample_format() {
                SampleFormat::U16 => {
                    assert_close(&img, &test_image_u16(ColorType::Rgb), 6.0 * 257.0, name);
                }
                // ADR-0005 D5 allows a backend that cannot produce more
                // than 8 bits to return 8; the pixels still have to be
                // right.
                SampleFormat::U8 => {
                    eprintln!("{name}: 10-bit source returned as 8-bit samples");
                    assert_close(&img, &test_image(ColorType::Rgb), 6.0, name);
                }
                SampleFormat::F32 => panic!("{name}: float samples from HEVC"),
            }
        });
    }

    #[test]
    fn pixel_limit() {
        for_each_backend(|d| {
            let opts = DecodeOpts {
                max_pixels: u64::from(W * H) - 1,
                ..Default::default()
            };
            assert!(
                matches!(
                    d.decode(&fixture("pattern-rgb.heic"), &opts),
                    Err(Error::TooLarge { pixels, .. }) if pixels == u64::from(W * H)
                ),
                "{}: pixel limit not enforced",
                d.caps().name
            );
        });
        // And through the registry, whichever backend it picks.
        let reg = registry();
        if reg.has_decoder(Format::Heic) {
            assert_too_large(&reg, "pattern-rgb.heic");
        }
    }

    #[test]
    fn corrupt_file_is_a_codec_error() {
        for_each_backend(|d| {
            let mut bytes = fixture("pattern-rgb.heic");
            let len = bytes.len();
            bytes.truncate(len / 2);
            let err = d.decode(&bytes, &DecodeOpts::default()).unwrap_err();
            assert!(matches!(err, Error::Codec(_)), "{}: {err}", d.caps().name);
        });
    }

    #[test]
    fn registry_reports_what_it_has() {
        let reg = registry();
        let bytes = fixture("pattern-rgb.heic");
        assert_eq!(reg.identify(&bytes).map(|i| i.format), Some(Format::Heic));
        match reg.decode(&bytes, &DecodeOpts::default()) {
            Ok(out) => {
                assert!(reg.has_decoder(Format::Heic));
                assert_eq!(out.info.format, Format::Heic);
            }
            Err(Error::DecoderUnavailable {
                format: Format::Heic,
                available_in: &["native-heif"],
                reason: Some(reason),
            }) => {
                let windows = cfg!(windows);
                assert!(windows, "no usable HEIC decoder here: {reason}");
                assert!(!reg.has_decoder(Format::Heic));
                for d in reg.decoders().filter(|d| d.caps().format == Format::Heic) {
                    assert!(reason.contains(d.caps().name), "{reason}");
                }
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
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

// ------------------------------------------------------------------ GIF

#[cfg(feature = "gif")]
mod gif {
    use super::*;

    /// The pattern has more colours than a GIF palette holds, so the
    /// fixtures are quantised and the comparison is bounded.
    const QUANTISED: f64 = 8.0;

    #[test]
    fn a_still_opaque_frame_is_rgb() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.gif");
        assert_eq!(info.format, Format::Gif);
        assert!(!info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_close(
            &img,
            &test_image(ColorType::Rgb),
            QUANTISED,
            "pattern-rgb.gif",
        );
    }

    #[test]
    fn a_transparent_index_makes_rgba() {
        let (img, _) = decode(&registry(), "pattern-alpha.gif");
        assert_eq!(img.color(), ColorType::Rgba);
        let s = img.samples().as_u8().unwrap();
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let want = if y % 2 == 0 { 255 } else { 0 };
                assert_eq!(s[i + 3], want, "alpha at {x},{y}");
            }
        }
    }

    #[test]
    fn animation_is_reported_and_the_first_frame_decoded() {
        let (img, info) = decode(&registry(), "pattern-anim.gif");
        assert!(info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_close(
            &img,
            &test_image(ColorType::Rgb),
            QUANTISED,
            "pattern-anim.gif",
        );
    }

    #[test]
    fn pixel_limit() {
        assert_too_large(&registry(), "pattern-rgb.gif");
    }
}

// ----------------------------------------------------------------- TIFF

#[cfg(feature = "tiff")]
mod tiff {
    use super::*;

    #[test]
    fn rgb_uncompressed() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.tif");
        assert_eq!(info.format, Format::Tiff);
        assert!(!info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_eq!(img.icc(), None);
        assert_pixels(&img, &test_image(ColorType::Rgb));
    }

    #[test]
    fn rgba_lzw() {
        let (img, _) = decode(&registry(), "pattern-rgba.tif");
        assert_pixels(&img, &test_image(ColorType::Rgba));
    }

    #[test]
    fn gray_stays_gray() {
        let (img, _) = decode(&registry(), "pattern-gray.tif");
        assert_pixels(&img, &test_image(ColorType::Gray));
    }

    #[test]
    fn sixteen_bit_stays_wide() {
        let (img, _) = decode(&registry(), "pattern-rgb16.tif");
        assert_eq!(img.sample_format(), SampleFormat::U16);
        assert_pixels(&img, &test_image_u16(ColorType::Rgb));
    }

    /// TIFF's EXIF is the file's own IFD, not a blob, so only the XMP tag
    /// rides along.
    #[test]
    fn xmp_rides_on_the_image() {
        let (img, _) = decode(&registry(), "pattern-meta.tif");
        assert_pixels(&img, &test_image(ColorType::Rgb));
        assert!(contains(img.xmp().unwrap(), b"test pattern"));
        assert_eq!(img.exif(), None);
    }

    #[test]
    fn icc_is_kept() {
        let (img, _) = decode(&registry(), "pattern-icc.tif");
        assert!(img.icc().is_some_and(is_icc), "ICC profile missing");
        assert_eq!(img.with_icc(None), test_image(ColorType::Rgb));
    }

    #[test]
    fn orientation_tag_is_applied_unless_disabled() {
        let reg = registry();
        let (img, _) = decode(&reg, "pattern-rot90.tif");
        assert_eq!((img.width(), img.height()), (W, H));
        assert_pixels(&img, &test_image(ColorType::Rgb));

        let raw = reg
            .decode(
                &fixture("pattern-rot90.tif"),
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
        assert_too_large(&registry(), "pattern-rgb.tif");
    }
}

// ------------------------------------------- BMP, TGA, ICO, QOI, PNM

#[cfg(feature = "bmp")]
mod bmp {
    use super::*;

    #[test]
    fn rgb_and_rgba() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.bmp");
        assert_eq!(info.format, Format::Bmp);
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.bmp");
        assert_pixels(&img, &test_image(ColorType::Rgba));
        assert_too_large(&reg, "pattern-rgb.bmp");
    }
}

#[cfg(feature = "tga")]
mod tga {
    use super::*;

    #[test]
    fn rgb_rgba_and_gray() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.tga");
        assert_eq!(info.format, Format::Tga);
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.tga");
        assert_pixels(&img, &test_image(ColorType::Rgba));
        let (img, _) = decode(&reg, "pattern-gray.tga");
        assert_pixels(&img, &test_image(ColorType::Gray));
        assert_too_large(&reg, "pattern-rgb.tga");
    }

    /// TGA has no magic number, so every other fixture must be claimed by
    /// its own decoder, never by the TGA plausibility check.
    #[test]
    fn no_other_fixture_looks_like_a_tga() {
        let reg = registry();
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "tga" || e == "md") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            if let Some(info) = reg.identify(&bytes) {
                assert_ne!(info.format, Format::Tga, "{}", path.display());
            }
        }
    }
}

#[cfg(feature = "ico")]
mod ico {
    use super::*;

    #[test]
    fn rgba_png_entry() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgba.ico");
        assert_eq!(info.format, Format::Ico);
        assert_pixels(&img, &test_image(ColorType::Rgba));
        assert_too_large(&reg, "pattern-rgba.ico");
    }
}

#[cfg(feature = "qoi")]
mod qoi {
    use super::*;

    #[test]
    fn rgb_and_rgba() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.qoi");
        assert_eq!(info.format, Format::Qoi);
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-rgba.qoi");
        assert_pixels(&img, &test_image(ColorType::Rgba));
        assert_too_large(&reg, "pattern-rgb.qoi");
    }
}

#[cfg(feature = "pnm")]
mod pnm {
    use super::*;

    #[test]
    fn binary_ascii_gray_and_sixteen_bit() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.ppm");
        assert_eq!(info.format, Format::Pnm);
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-ascii.ppm");
        assert_pixels(&img, &test_image(ColorType::Rgb));
        let (img, _) = decode(&reg, "pattern-gray.pgm");
        assert_pixels(&img, &test_image(ColorType::Gray));
        let (img, _) = decode(&reg, "pattern-rgb16.ppm");
        assert_eq!(img.sample_format(), SampleFormat::U16);
        assert_pixels(&img, &test_image_u16(ColorType::Rgb));
        assert_too_large(&reg, "pattern-rgb.ppm");
    }
}

// ------------------------------------------------------------------ SVG

#[cfg(feature = "svg")]
mod svg {
    use super::*;

    #[test]
    fn one_rect_per_pixel_renders_the_pattern() {
        let reg = registry();
        let (img, info) = decode(&reg, "pattern-rgb.svg");
        assert_eq!(info.format, Format::Svg);
        assert!(!info.animated);
        assert_eq!(img.color(), ColorType::Rgb);
        assert_pixels(&img, &test_image(ColorType::Rgb));
        assert_too_large(&reg, "pattern-rgb.svg");
    }

    #[test]
    fn translucent_rows_come_back_unpremultiplied() {
        let (img, _) = decode(&registry(), "pattern-rgba.svg");
        assert_eq!(img.color(), ColorType::Rgba);
        // Premultiplying and undoing it at 8 bits costs up to a step.
        assert_close(&img, &test_image(ColorType::Rgba), 1.0, "pattern-rgba.svg");
        let err = mae_channel(&img, &test_image(ColorType::Rgba), 3);
        assert!(err <= 1.0, "alpha error {err}");
    }
}
