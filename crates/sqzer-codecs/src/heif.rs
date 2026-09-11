//! HEIC container parsing that every build gets, decoder or not: the brand
//! sniff, and a walk of the `meta` box for what the primary item looks
//! like before a single pixel is decoded. The HEIC backends in
//! `native::heif` use it for `probe`, for `dimensions`, for the
//! orientation they apply and for the ICC profile they attach, so the
//! three of them agree by construction (ADR-0005 D6). A build with none of
//! them registers [`probe`] as a sniffer so a HEIC input is reported as
//! "needs `native-heif`" rather than "unrecognised".
//!
//! Only what the backends need is read: `ftyp`, `pitm`, and the primary
//! item's `ispe`, `clap`, `irot`, `imir` and `colr` properties. Item
//! locations, the HEVC configuration and auxiliary items stay with the
//! decoders. Everything is bounds-checked and a malformed file yields
//! `None`; the decoder then produces the real error.

use sqzer_core::codec::{Format, FormatInfo};
use sqzer_core::image::Orientation;

/// Brands of HEVC-coded HEIF files, still images and sequences. AVIF
/// brands are left to the AVIF decoder on purpose.
const STILL_BRANDS: [&[u8; 4]; 4] = [b"heic", b"heix", b"heim", b"heis"];
const SEQUENCE_BRANDS: [&[u8; 4]; 4] = [b"hevc", b"hevx", b"hevm", b"hevs"];

/// The brand sniff: `Some` for an HEVC-coded HEIF, `None` for anything
/// else, AVIF included. Also the [`sqzer_core::registry::Sniffer`] a
/// build without a HEIC decoder registers.
#[must_use]
pub fn probe(bytes: &[u8]) -> Option<FormatInfo> {
    let (kind, body) = boxes(bytes).next()?;
    if kind != b"ftyp" || body.len() < 8 {
        return None;
    }
    // [major:4][minor:4][compatible:4]*
    let major = &body[..4];
    let compatible = body[8..].as_chunks::<4>().0;
    let brands = std::iter::once(major).chain(compatible.iter().map(<[u8; 4]>::as_slice));
    let is_still = |b: &[u8]| STILL_BRANDS.iter().any(|s| s.as_slice() == b);
    let is_sequence = |b: &[u8]| SEQUENCE_BRANDS.iter().any(|s| s.as_slice() == b);
    let mut still = false;
    let mut sequence = false;
    for brand in brands {
        still |= is_still(brand);
        sequence |= is_sequence(brand);
    }
    (still || sequence).then_some(FormatInfo {
        format: Format::Heic,
        animated: is_sequence(major),
    })
}

/// What the container says about the primary image, without decoding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Displayed width: the clean aperture, with the axes swapped when the
    /// rotation swaps them.
    pub width: u32,
    /// Displayed height.
    pub height: u32,
    /// The transform that brings the decoded, cropped frame upright:
    /// `irot` and `imir` composed in the order the file lists them.
    pub orientation: Orientation,
    /// The ICC profile of a `prof` or `rICC` colour box. An `nclx` box is
    /// ignored, as `libheif` ignores it (ADR-0004).
    pub icc: Option<Vec<u8>>,
}

/// Read the [`Header`] of a HEIC. `None` when the bytes are not a HEIC or
/// the `meta` box does not describe a primary item with a size.
#[must_use]
pub fn header(bytes: &[u8]) -> Option<Header> {
    probe(bytes)?;
    // `meta` is a FullBox: version and flags precede its children.
    let meta = boxes(bytes)
        .find(|(kind, _)| *kind == b"meta")?
        .1
        .get(4..)?;
    let mut primary = None;
    let mut ipco = None;
    let mut ipma = None;
    for (kind, body) in boxes(meta) {
        match kind {
            b"pitm" => primary = primary_item(body),
            b"iprp" => {
                for (kind, body) in boxes(body) {
                    match kind {
                        b"ipco" => ipco = Some(body),
                        b"ipma" => ipma = Some(body),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let properties: Vec<_> = boxes(ipco?).collect();
    let mut ispe = None;
    let mut aperture = None;
    let mut orientation = Orientation::Normal;
    let mut icc = None;
    for index in associations(ipma?, primary?) {
        // Property indices are 1-based; 0 means "none".
        let Some((kind, body)) = index
            .checked_sub(1)
            .and_then(|i| properties.get(usize::from(i)))
        else {
            continue;
        };
        match *kind {
            b"ispe" => ispe = ispe_size(body),
            b"clap" => aperture = clap_size(body),
            b"irot" => orientation = orientation.then(irot(body)),
            b"imir" => orientation = orientation.then(imir(body)),
            b"colr" => {
                if let Some(profile) = body
                    .strip_prefix(b"prof")
                    .or_else(|| body.strip_prefix(b"rICC"))
                {
                    icc = Some(profile.to_vec());
                }
            }
            _ => {}
        }
    }
    let (width, height) = ispe?;
    let (width, height) = aperture.map_or((width, height), |(w, h)| (w.min(width), h.min(height)));
    let (width, height) = if orientation.swaps_axes() {
        (height, width)
    } else {
        (width, height)
    };
    Some(Header {
        width,
        height,
        orientation,
        icc,
    })
}

/// The boxes laid end to end in `data`, as `(type, body)`. Stops at the
/// first box that does not fit.
fn boxes(mut data: &[u8]) -> impl Iterator<Item = (&[u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        let (kind, body, rest) = split_box(data)?;
        data = rest;
        Some((kind, body))
    })
}

/// One ISOBMFF box: `[size:4][type:4]`, with `size == 1` meaning a 64-bit
/// size follows and `size == 0` meaning "to the end".
fn split_box(data: &[u8]) -> Option<(&[u8; 4], &[u8], &[u8])> {
    let size32 = be32(data)?;
    let kind: &[u8; 4] = data.get(4..8)?.try_into().ok()?;
    let (header, size) = match size32 {
        0 => (8, data.len()),
        1 => (16, usize::try_from(be64(data.get(8..)?)?).ok()?),
        n => (8, usize::try_from(n).ok()?),
    };
    if size < header || size > data.len() {
        return None;
    }
    Some((kind, &data[header..size], &data[size..]))
}

/// `pitm`: the primary item's ID. A `FullBox`; version 0 stores 16 bits.
fn primary_item(body: &[u8]) -> Option<u32> {
    match *body.first()? {
        0 => be16(body.get(4..)?).map(u32::from),
        _ => be32(body.get(4..)?),
    }
}

/// `ipma`: the property indices associated with `item`, in the order the
/// file lists them, which is the order transformative properties apply.
fn associations(body: &[u8], item: u32) -> Vec<u16> {
    fn walk(body: &[u8], item: u32) -> Option<Vec<u16>> {
        let version = *body.first()?;
        let wide_index = body.get(3)? & 1 == 1;
        let entries = be32(body.get(4..)?)?;
        let mut at = 8;
        for _ in 0..entries {
            let id = if version == 0 {
                let id = u32::from(be16(body.get(at..)?)?);
                at += 2;
                id
            } else {
                let id = be32(body.get(at..)?)?;
                at += 4;
                id
            };
            let count = usize::from(*body.get(at)?);
            at += 1;
            let mut indices = Vec::with_capacity(count);
            for _ in 0..count {
                // The top bit flags the property as essential.
                let index = if wide_index {
                    let v = be16(body.get(at..)?)?;
                    at += 2;
                    v & 0x7fff
                } else {
                    let v = *body.get(at)?;
                    at += 1;
                    u16::from(v & 0x7f)
                };
                indices.push(index);
            }
            if id == item {
                return Some(indices);
            }
        }
        None
    }
    walk(body, item).unwrap_or_default()
}

/// `ispe`: a `FullBox` holding the coded width and height.
fn ispe_size(body: &[u8]) -> Option<(u32, u32)> {
    let width = be32(body.get(4..)?)?;
    let height = be32(body.get(8..)?)?;
    (width > 0 && height > 0).then_some((width, height))
}

/// `clap`: the clean aperture as four fractions; only the size matters
/// here, rounded to the nearest pixel as `libheif` rounds it.
fn clap_size(body: &[u8]) -> Option<(u32, u32)> {
    let fraction = |at: usize| -> Option<u32> {
        let n = u64::from(be32(body.get(at..)?)?);
        let d = u64::from(be32(body.get(at + 4..)?)?);
        (d > 0)
            .then(|| u32::try_from((n + d / 2) / d).ok())
            .flatten()
    };
    let width = fraction(0)?;
    let height = fraction(8)?;
    (width > 0 && height > 0).then_some((width, height))
}

/// `irot`: `angle` quarter turns anticlockwise. The EXIF numbering is
/// clockwise, so one quarter turn anticlockwise is `Rotate270`.
fn irot(body: &[u8]) -> Orientation {
    match body.first().map_or(0, |b| b & 3) {
        1 => Orientation::Rotate270,
        2 => Orientation::Rotate180,
        3 => Orientation::Rotate90,
        _ => Orientation::Normal,
    }
}

/// `imir`: `axis` 0 exchanges top and bottom, 1 exchanges left and right
/// (ISO/IEC 23008-12:2022 6.5.12, which is also how `libheif` reads it).
fn imir(body: &[u8]) -> Orientation {
    match body.first().map_or(0, |b| b & 1) {
        0 => Orientation::FlipVertical,
        _ => Orientation::FlipHorizontal,
    }
}

fn be16(b: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}

fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

fn be64(b: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let size = u32::try_from(8 + body.len()).unwrap();
        let mut b = size.to_be_bytes().to_vec();
        b.extend_from_slice(&kind);
        b.extend_from_slice(body);
        b
    }

    fn full(kind: [u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut inner = vec![version];
        inner.extend_from_slice(&flags.to_be_bytes()[1..]);
        inner.extend_from_slice(body);
        bx(kind, &inner)
    }

    fn ftyp(major: [u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut body = major.to_vec();
        body.extend_from_slice(&[0, 0, 0, 0]);
        for c in compatible {
            body.extend_from_slice(*c);
        }
        bx(*b"ftyp", &body)
    }

    fn ispe(w: u32, h: u32) -> Vec<u8> {
        let mut body = w.to_be_bytes().to_vec();
        body.extend_from_slice(&h.to_be_bytes());
        full(*b"ispe", 0, 0, &body)
    }

    fn clap(w: u32, h: u32) -> Vec<u8> {
        let mut body = Vec::new();
        for v in [w, 1, h, 1, 0, 1, 0, 1] {
            body.extend_from_slice(&v.to_be_bytes());
        }
        bx(*b"clap", &body)
    }

    /// A HEIC skeleton: `ftyp`, then a `meta` with the given property
    /// boxes all associated with primary item 1 in the order given.
    fn heic(properties: &[Vec<u8>]) -> Vec<u8> {
        let mut file = ftyp(*b"heic", &[b"mif1", b"heic"]);
        let mut ipco = Vec::new();
        for p in properties {
            ipco.extend_from_slice(p);
        }
        let mut ipma = 1u32.to_be_bytes().to_vec();
        ipma.extend_from_slice(&1u16.to_be_bytes());
        ipma.push(u8::try_from(properties.len()).unwrap());
        for i in 1..=properties.len() {
            ipma.push(0x80 | u8::try_from(i).unwrap());
        }
        let iprp = bx(
            *b"iprp",
            &[bx(*b"ipco", &ipco), full(*b"ipma", 0, 0, &ipma)].concat(),
        );
        let pitm = full(*b"pitm", 0, 0, &1u16.to_be_bytes());
        file.extend_from_slice(&full(*b"meta", 0, 0, &[pitm, iprp].concat()));
        file
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn probe_reads_hevc_brands_only() {
        let heic = probe(&ftyp(*b"heic", &[b"mif1", b"heic"]));
        assert_eq!(
            heic,
            Some(FormatInfo {
                format: Format::Heic,
                animated: false
            })
        );
        // Generic major brand, HEIC only among the compatible ones.
        assert!(probe(&ftyp(*b"mif1", &[b"heic"])).is_some());
        // A sequence is animated.
        assert_eq!(
            probe(&ftyp(*b"hevc", &[b"mif1", b"msf1"])).map(|i| i.animated),
            Some(true)
        );
        // AVIF stays with the AVIF decoder, and other ISOBMFF is not ours.
        assert!(probe(&ftyp(*b"avif", &[b"mif1", b"miaf"])).is_none());
        assert!(probe(&ftyp(*b"isom", &[b"mp42"])).is_none());
        assert!(probe(b"\x89PNG\r\n\x1a\n").is_none());
        assert!(probe(b"").is_none());
    }

    #[test]
    fn fixtures_report_the_displayed_size() {
        for name in [
            "pattern-rgb.heic",
            "pattern-rgba.heic",
            "pattern-gray.heic",
            "pattern-icc.heic",
            "pattern-rot90.heic",
        ] {
            let h = header(&fixture(name)).unwrap_or_else(|| panic!("{name}: no header"));
            assert_eq!((h.width, h.height), (48, 32), "{name}");
            let expected = if name.contains("rot90") {
                Orientation::Rotate90
            } else {
                Orientation::Normal
            };
            assert_eq!(h.orientation, expected, "{name}");
            if name.contains("icc") {
                let icc = h.icc.expect("profile");
                assert!(icc.len() > 128 && &icc[36..40] == b"acsp", "{name}: ICC");
            } else {
                assert_eq!(h.icc, None, "{name}");
            }
        }
        // Not ours: the AVIF twin of the pattern is left alone.
        assert_eq!(header(&fixture("pattern-rgb.avif")), None);
    }

    #[test]
    fn transforms_compose_in_file_order() {
        let h = header(&heic(&[ispe(64, 64), clap(48, 32)])).unwrap();
        assert_eq!(
            (h.width, h.height, h.orientation),
            (48, 32, Orientation::Normal)
        );

        // One quarter turn anticlockwise, listed after the crop.
        let h = header(&heic(&[ispe(64, 64), clap(48, 32), bx(*b"irot", &[1])])).unwrap();
        assert_eq!(
            (h.width, h.height, h.orientation),
            (32, 48, Orientation::Rotate270)
        );

        // Mirror, then rotate: the composition, not either alone.
        let h = header(&heic(&[
            ispe(48, 32),
            bx(*b"imir", &[1]),
            bx(*b"irot", &[3]),
        ]))
        .unwrap();
        assert_eq!(
            h.orientation,
            Orientation::FlipHorizontal.then(Orientation::Rotate90)
        );
        assert_eq!((h.width, h.height), (32, 48));
        let h = header(&heic(&[ispe(48, 32), bx(*b"imir", &[0])])).unwrap();
        assert_eq!(h.orientation, Orientation::FlipVertical);
        let h = header(&heic(&[ispe(48, 32), bx(*b"irot", &[2])])).unwrap();
        assert_eq!(
            (h.width, h.height, h.orientation),
            (48, 32, Orientation::Rotate180)
        );
    }

    #[test]
    fn colour_box_yields_the_profile_bytes() {
        let mut colr = b"prof".to_vec();
        colr.extend_from_slice(b"not really a profile");
        let h = header(&heic(&[ispe(8, 8), bx(*b"colr", &colr)])).unwrap();
        assert_eq!(h.icc.as_deref(), Some(&b"not really a profile"[..]));
        let mut nclx = b"nclx".to_vec();
        nclx.extend_from_slice(&[0, 1, 0, 13, 0, 6, 0x80]);
        let h = header(&heic(&[ispe(8, 8), bx(*b"colr", &nclx)])).unwrap();
        assert_eq!(h.icc, None);
    }

    #[test]
    fn malformed_containers_yield_none() {
        // No `ispe` at all.
        assert_eq!(header(&heic(&[clap(4, 4)])), None);
        // A truncated file: the brands are there, the meta box is cut.
        let file = heic(&[ispe(8, 8)]);
        assert_eq!(header(&file[..file.len() - 10]), None);
        // A box claiming to be larger than the file.
        let mut file = ftyp(*b"heic", &[b"mif1"]);
        file.extend_from_slice(&[0, 0, 1, 0]);
        file.extend_from_slice(b"meta");
        assert_eq!(header(&file), None);
        // A clean aperture wider than the image is clamped, not trusted.
        let h = header(&heic(&[ispe(8, 8), clap(100, 100)])).unwrap();
        assert_eq!((h.width, h.height), (8, 8));
    }
}
