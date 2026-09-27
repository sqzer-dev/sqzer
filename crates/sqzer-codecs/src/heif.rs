//! HEIF container parsing that every build gets, decoder or not: the HEIC
//! brand sniff, and a walk of the `meta` box for what the primary item
//! looks like before a single pixel is decoded. The HEIC backends in
//! `native::heif` use it for `probe`, for `dimensions`, for the
//! orientation they apply and for the metadata they attach, so the three
//! of them agree by construction (ADR-0005 D6). AVIF is a HEIF too, and
//! its decoder takes the same walk through [`read`]. A build with no HEIC
//! decoder registers [`probe`] as a sniffer so a HEIC input is reported as
//! "needs `native-heif`" rather than "unrecognised".
//!
//! Only what the backends need is read: `ftyp`, `pitm`, the primary item's
//! `ispe`, `clap`, `irot`, `imir` and `colr` properties, and the Exif and
//! XMP items that describe it (`iinf`, `iref`, `iloc`, `idat`). The coded
//! payload, its configuration and auxiliary items stay with the decoders.
//! Everything is bounds-checked and a malformed file yields `None`, or no
//! metadata; the decoder then produces the real error.

use sqzer_core::codec::{Format, FormatInfo};
use sqzer_core::image::{Metadata, Orientation, reset_exif_orientation};

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
    /// The ICC profile of a `prof` or `rICC` colour box.
    pub icc: Option<Vec<u8>>,
    /// The code points of an `nclx` colour box. The HEIC backends ignore
    /// it, as `libheif` does (ADR-0004); the AVIF decoder reads the
    /// primaries from it.
    pub nclx: Option<Nclx>,
    /// The Exif item describing the primary item, as a TIFF structure
    /// from the byte-order mark. Its `Orientation` tag is informative only:
    /// `irot` and `imir` are what rotate a HEIF.
    pub exif: Option<Vec<u8>>,
    /// The XMP item describing the primary item, the XML bytes.
    pub xmp: Option<Vec<u8>>,
}

impl Header {
    /// The ICC profile, Exif and XMP to attach to the decoded image. The
    /// Exif `Orientation` tag is reset to 1: in a HEIF the container's
    /// `irot` and `imir` rotate the picture, and a kept tag would rotate it
    /// a second time in the next viewer.
    #[must_use]
    pub fn metadata(&self) -> Metadata {
        let mut exif = self.exif.clone();
        if let Some(exif) = &mut exif {
            reset_exif_orientation(exif);
        }
        Metadata {
            icc: self.icc.clone(),
            exif,
            xmp: self.xmp.clone(),
        }
    }
}

/// Colour code points (ITU-T H.273) from an `nclx` colour box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nclx {
    /// `colour_primaries`: 1 is BT.709 and sRGB, 12 Display P3, 9 BT.2020.
    pub primaries: u16,
    /// `transfer_characteristics`: 13 is sRGB, 16 PQ, 18 HLG.
    pub transfer: u16,
    /// `matrix_coefficients`.
    pub matrix: u16,
    /// Full-range samples rather than limited ("video") range.
    pub full_range: bool,
}

/// Read the [`Header`] of a HEIC. `None` when the bytes are not a HEIC or
/// the `meta` box does not describe a primary item with a size.
#[must_use]
pub fn header(bytes: &[u8]) -> Option<Header> {
    probe(bytes)?;
    read(bytes)
}

/// Read the [`Header`] of any HEIF, whatever its brand: the AVIF decoder's
/// entry point, since [`header`] rejects everything but HEIC. `None` when
/// the `meta` box does not describe a primary item with a size.
#[must_use]
pub fn read(bytes: &[u8]) -> Option<Header> {
    // `meta` is a FullBox: version and flags precede its children.
    let meta = boxes(bytes)
        .find(|(kind, _)| *kind == b"meta")?
        .1
        .get(4..)?;
    let mut primary = None;
    let mut ipco = None;
    let mut ipma = None;
    let mut items = Items::default();
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
            b"iinf" => items.iinf = Some(body),
            b"iref" => items.iref = Some(body),
            b"iloc" => items.iloc = Some(body),
            b"idat" => items.idat = Some(body),
            _ => {}
        }
    }
    let primary = primary?;
    let properties: Vec<_> = boxes(ipco?).collect();
    let mut ispe = None;
    let mut aperture = None;
    let mut orientation = Orientation::Normal;
    let mut icc = None;
    let mut nclx = None;
    for index in associations(ipma?, primary) {
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
                } else if let Some(points) = body.strip_prefix(b"nclx") {
                    nclx = nclx_points(points);
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
        nclx,
        exif: items.exif(bytes, primary),
        xmp: items.xmp(bytes, primary),
    })
}

/// `nclx`: primaries, transfer and matrix as 16-bit code points, then the
/// full-range flag in the top bit of one byte.
fn nclx_points(body: &[u8]) -> Option<Nclx> {
    Some(Nclx {
        primaries: be16(body)?,
        transfer: be16(body.get(2..)?)?,
        matrix: be16(body.get(4..)?)?,
        full_range: body.get(6)? & 0x80 != 0,
    })
}

/// The item boxes of `meta` that metadata lookups need.
#[derive(Default)]
struct Items<'a> {
    iinf: Option<&'a [u8]>,
    iref: Option<&'a [u8]>,
    iloc: Option<&'a [u8]>,
    idat: Option<&'a [u8]>,
}

/// One `infe` entry: an item's ID and type, and for a `mime` item its
/// content type and encoding.
struct ItemInfo<'a> {
    id: u32,
    kind: [u8; 4],
    content_type: &'a [u8],
    content_encoding: &'a [u8],
}

impl Items<'_> {
    /// The Exif item describing `primary`, as TIFF from the byte-order
    /// mark. The item payload starts with a 32-bit offset to the TIFF
    /// header, past whatever preamble (usually `Exif\0\0`) the writer put
    /// first.
    fn exif(&self, file: &[u8], primary: u32) -> Option<Vec<u8>> {
        let item = self.find(primary, |i| &i.kind == b"Exif")?;
        let payload = self.payload(file, item)?;
        let skip = usize::try_from(be32(&payload)?).ok()?;
        let tiff = payload.get(4usize.checked_add(skip)?..)?;
        let tiff = tiff.strip_prefix(b"Exif\0\0").unwrap_or(tiff);
        (tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*")).then(|| tiff.to_vec())
    }

    /// The XMP item describing `primary`: a `mime` item of type
    /// `application/rdf+xml`. A content-encoded packet is skipped rather
    /// than inflated.
    fn xmp(&self, file: &[u8], primary: u32) -> Option<Vec<u8>> {
        let item = self.find(primary, |i| {
            &i.kind == b"mime"
                && i.content_type == b"application/rdf+xml"
                && i.content_encoding.is_empty()
        })?;
        self.payload(file, item)
    }

    /// The first item matching `want` that describes `primary`: a `cdsc`
    /// reference from the item to the primary one, or any matching item
    /// when the file has no `cdsc` references at all.
    fn find(&self, primary: u32, want: impl Fn(&ItemInfo<'_>) -> bool) -> Option<u32> {
        let described = cdsc_references(self.iref.unwrap_or_default());
        let infos = item_infos(self.iinf?);
        infos
            .iter()
            .filter(|i| want(i))
            .find(|i| {
                described.is_empty()
                    || described
                        .iter()
                        .any(|(from, to)| *from == i.id && to.contains(&primary))
            })
            .map(|i| i.id)
    }

    /// The bytes of `item`, its `iloc` extents concatenated.
    fn payload(&self, file: &[u8], item: u32) -> Option<Vec<u8>> {
        item_payload(self.iloc?, item, file, self.idat.unwrap_or_default())
    }
}

/// `iinf`: the `infe` entries of version 2 and 3, the ones that carry an
/// item type. Older entries have no type and are skipped.
fn item_infos(body: &[u8]) -> Vec<ItemInfo<'_>> {
    let entries_at = if *body.first().unwrap_or(&0) == 0 {
        6
    } else {
        8
    };
    let Some(entries) = body.get(entries_at..) else {
        return Vec::new();
    };
    boxes(entries)
        .filter(|(kind, _)| *kind == b"infe")
        .filter_map(|(_, infe)| item_info(infe))
        .collect()
}

/// One `infe` `FullBox`: version 2 has a 16-bit item ID, version 3 a 32-bit
/// one, then a protection index and the item type.
fn item_info(body: &[u8]) -> Option<ItemInfo<'_>> {
    let version = *body.first()?;
    let (id, at) = match version {
        2 => (u32::from(be16(body.get(4..)?)?), 6),
        3 => (be32(body.get(4..)?)?, 8),
        _ => return None,
    };
    // Skip `item_protection_index`.
    let kind: [u8; 4] = body.get(at + 2..at + 6)?.try_into().ok()?;
    let mut rest = body.get(at + 6..)?;
    // `item_name`, then for `mime` the content type and encoding, each a
    // NUL-terminated string; a missing trailing string reads as empty.
    let mut next = || {
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        let s = &rest[..end];
        rest = rest.get(end + 1..).unwrap_or_default();
        s
    };
    let _name = next();
    let (content_type, content_encoding) = if &kind == b"mime" {
        (next(), next())
    } else {
        (&[][..], &[][..])
    };
    Some(ItemInfo {
        id,
        kind,
        content_type,
        content_encoding,
    })
}

/// `iref`: every `cdsc` ("content describes") reference, as the describing
/// item and the items it describes.
fn cdsc_references(body: &[u8]) -> Vec<(u32, Vec<u32>)> {
    let Some(&version) = body.first() else {
        return Vec::new();
    };
    let id = |b: &[u8], at: usize| -> Option<(u32, usize)> {
        if version == 0 {
            Some((u32::from(be16(b.get(at..)?)?), at + 2))
        } else {
            Some((be32(b.get(at..)?)?, at + 4))
        }
    };
    let reference = |b: &[u8]| -> Option<(u32, Vec<u32>)> {
        let (from, mut at) = id(b, 0)?;
        let count = be16(b.get(at..)?)?;
        at += 2;
        let mut to = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let (target, next) = id(b, at)?;
            to.push(target);
            at = next;
        }
        Some((from, to))
    };
    boxes(body.get(4..).unwrap_or_default())
        .filter(|(kind, _)| *kind == b"cdsc")
        .filter_map(|(_, b)| reference(b))
        .collect()
}

/// `iloc`: the bytes of `item`, versions 0 to 2. Construction method 0
/// reads from the file, 1 from the `idat` box; method 2, item offsets, is
/// not followed. A zero extent length means "to the end".
fn item_payload(body: &[u8], item: u32, file: &[u8], idat: &[u8]) -> Option<Vec<u8>> {
    let version = *body.first()?;
    let field_sizes = *body.get(4)?;
    let more = *body.get(5)?;
    let (offset_size, length_size) = (field_sizes >> 4, field_sizes & 15);
    let base_offset_size = more >> 4;
    let index_size = if version == 0 { 0 } else { more & 15 };
    let mut at = 6;
    // A 16-bit field, or a 32-bit one where the version widens it.
    let wide = |at: &mut usize, bits32: bool| -> Option<u32> {
        let v = if bits32 {
            be32(body.get(*at..)?)?
        } else {
            u32::from(be16(body.get(*at..)?)?)
        };
        *at += if bits32 { 4 } else { 2 };
        Some(v)
    };
    // A field whose width in bytes the header declares: 0, 4 or 8.
    let sized = |at: &mut usize, width: u8| -> Option<u64> {
        let v = match width {
            0 => 0,
            4 => u64::from(be32(body.get(*at..)?)?),
            8 => be64(body.get(*at..)?)?,
            _ => return None,
        };
        *at += usize::from(width);
        Some(v)
    };
    let items = wide(&mut at, version == 2)?;
    for _ in 0..items {
        let id = wide(&mut at, version == 2)?;
        let method = if version == 0 {
            0
        } else {
            wide(&mut at, false)? & 15
        };
        // `data_reference_index`: 0 is "this file", the only one read.
        let data_reference = wide(&mut at, false)?;
        let base = sized(&mut at, base_offset_size)?;
        let extents = wide(&mut at, false)?;
        let mut out = Vec::new();
        for _ in 0..extents {
            sized(&mut at, index_size)?;
            let offset = sized(&mut at, offset_size)?;
            let length = sized(&mut at, length_size)?;
            if id != item {
                continue;
            }
            let source = match (method, data_reference) {
                (0, 0) => file,
                (1, _) => idat,
                _ => return None,
            };
            let start = usize::try_from(base.checked_add(offset)?).ok()?;
            let end = if length == 0 {
                source.len()
            } else {
                start.checked_add(usize::try_from(length).ok()?)?
            };
            out.extend_from_slice(source.get(start..end)?);
        }
        if id == item {
            return Some(out);
        }
    }
    None
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

    /// A metadata item for [`heif_with`].
    struct Item {
        id: u32,
        kind: [u8; 4],
        content_type: &'static str,
        encoding: &'static str,
        payload: Vec<u8>,
    }

    fn exif_item(id: u32, payload: Vec<u8>) -> Item {
        Item {
            id,
            kind: *b"Exif",
            content_type: "",
            encoding: "",
            payload,
        }
    }

    fn xmp_item(id: u32, encoding: &'static str, payload: &[u8]) -> Item {
        Item {
            id,
            kind: *b"mime",
            content_type: "application/rdf+xml",
            encoding,
            payload: payload.to_vec(),
        }
    }

    /// An Exif item payload: the offset to the TIFF header, the `Exif\0\0`
    /// preamble it skips, then the TIFF.
    fn exif_payload(tiff: &[u8]) -> Vec<u8> {
        let mut p = 6u32.to_be_bytes().to_vec();
        p.extend_from_slice(b"Exif\0\0");
        p.extend_from_slice(tiff);
        p
    }

    /// A HEIF with `major` as its brand, primary item 1 with an `ispe` of
    /// 8 x 8, the metadata `items`, `cdsc` references `(from, to)`, and the
    /// payloads stored in `idat` (construction method 1) or in an `mdat`
    /// after `meta` (method 0) through an `iloc` of `version`.
    fn heif_with(
        major: [u8; 4],
        items: &[Item],
        cdsc: &[(u32, u32)],
        in_idat: bool,
        version: u8,
    ) -> Vec<u8> {
        let wide = version == 2 || items.iter().any(|i| i.id > 0xffff);
        let id = |v: u32| -> Vec<u8> {
            if wide {
                v.to_be_bytes().to_vec()
            } else {
                u16::try_from(v).unwrap().to_be_bytes().to_vec()
            }
        };
        let build = |mdat_at: u32| -> Vec<u8> {
            let mut file = ftyp(major, &[b"mif1", b"miaf"]);
            // Properties of the primary item, as `heic` writes them.
            let ipco = bx(*b"ipco", &ispe(8, 8));
            let mut ipma = 1u32.to_be_bytes().to_vec();
            ipma.extend_from_slice(&1u16.to_be_bytes());
            ipma.extend_from_slice(&[1, 0x81]);
            let iprp = bx(*b"iprp", &[ipco, full(*b"ipma", 0, 0, &ipma)].concat());
            let pitm = full(*b"pitm", 0, 0, &1u16.to_be_bytes());

            let mut entries = Vec::new();
            for item in items {
                let mut e = id(item.id);
                e.extend_from_slice(&0u16.to_be_bytes());
                e.extend_from_slice(&item.kind);
                e.push(0); // empty item_name
                if &item.kind == b"mime" {
                    e.extend_from_slice(item.content_type.as_bytes());
                    e.push(0);
                    e.extend_from_slice(item.encoding.as_bytes());
                    e.push(0);
                }
                entries.extend_from_slice(&full(*b"infe", if wide { 3 } else { 2 }, 0, &e));
            }
            let iinf = if wide {
                let mut b = u32::try_from(items.len()).unwrap().to_be_bytes().to_vec();
                b.extend_from_slice(&entries);
                full(*b"iinf", 1, 0, &b)
            } else {
                let mut b = u16::try_from(items.len()).unwrap().to_be_bytes().to_vec();
                b.extend_from_slice(&entries);
                full(*b"iinf", 0, 0, &b)
            };

            let mut refs = Vec::new();
            for &(from, to) in cdsc {
                let mut r = id(from);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&id(to));
                refs.extend_from_slice(&bx(*b"cdsc", &r));
            }
            let iref = full(*b"iref", u8::from(wide), 0, &refs);

            // offset_size 4, length_size 4, no base offset, no index.
            let mut iloc = vec![0x44, 0x00];
            if version == 2 {
                iloc.extend_from_slice(&u32::try_from(items.len()).unwrap().to_be_bytes());
            } else {
                iloc.extend_from_slice(&u16::try_from(items.len()).unwrap().to_be_bytes());
            }
            let mut offset = if in_idat { 0 } else { mdat_at };
            for item in items {
                iloc.extend_from_slice(&id(item.id));
                if version > 0 {
                    iloc.extend_from_slice(&u16::from(in_idat).to_be_bytes());
                }
                iloc.extend_from_slice(&0u16.to_be_bytes()); // data_reference_index
                iloc.extend_from_slice(&1u16.to_be_bytes()); // one extent
                iloc.extend_from_slice(&offset.to_be_bytes());
                let len = u32::try_from(item.payload.len()).unwrap();
                iloc.extend_from_slice(&len.to_be_bytes());
                offset += len;
            }
            let iloc = full(*b"iloc", version, 0, &iloc);

            let payloads: Vec<u8> = items.iter().flat_map(|i| i.payload.clone()).collect();
            let mut meta = [pitm, iprp, iinf, iref, iloc].concat();
            if in_idat {
                meta.extend_from_slice(&bx(*b"idat", &payloads));
            }
            file.extend_from_slice(&full(*b"meta", 0, 0, &meta));
            if !in_idat {
                file.extend_from_slice(&bx(*b"mdat", &payloads));
            }
            file
        };
        // The `mdat` payloads start after `meta` and the 8-byte `mdat`
        // header; the offsets do not change the length, so one dry run
        // finds them.
        let dry = build(0);
        let mdat_at = dry.len() - items.iter().map(|i| i.payload.len()).sum::<usize>();
        build(u32::try_from(mdat_at).unwrap())
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

    #[test]
    fn read_takes_any_brand_header_only_heic() {
        let file = heif_with(*b"avif", &[], &[], true, 0);
        assert_eq!(header(&file), None);
        let h = read(&file).unwrap();
        assert_eq!((h.width, h.height), (8, 8));
        assert_eq!((h.exif, h.xmp), (None, None));
    }

    #[test]
    fn exif_and_xmp_items_from_idat_and_from_the_file() {
        let tiff = crate::exif::tiff_with_orientation(6);
        for (in_idat, version) in [(true, 1), (false, 0), (false, 1), (true, 2)] {
            let file = heif_with(
                *b"avif",
                &[
                    exif_item(2, exif_payload(&tiff)),
                    xmp_item(3, "", b"<x:xmpmeta/>"),
                ],
                &[(2, 1), (3, 1)],
                in_idat,
                version,
            );
            let what = format!("idat {in_idat}, iloc v{version}");
            let h = read(&file).unwrap_or_else(|| panic!("{what}"));
            assert_eq!(h.exif.as_deref(), Some(&tiff[..]), "{what}");
            assert_eq!(h.xmp.as_deref(), Some(&b"<x:xmpmeta/>"[..]), "{what}");
            // The container rotates a HEIF; the kept tag must not.
            let meta = h.metadata();
            assert_eq!(
                crate::exif::orientation(meta.exif.as_deref().unwrap()),
                Some(Orientation::Normal),
                "{what}"
            );
        }
    }

    #[test]
    fn wide_item_ids_are_read() {
        let file = heif_with(
            *b"heic",
            &[exif_item(
                0x1_0002,
                exif_payload(&crate::exif::tiff_with_orientation(1)),
            )],
            &[(0x1_0002, 1)],
            true,
            2,
        );
        assert!(header(&file).unwrap().exif.is_some());
    }

    #[test]
    fn only_items_describing_the_primary_count() {
        let tiff = crate::exif::tiff_with_orientation(1);
        // The Exif item describes item 7, not the primary item 1.
        let file = heif_with(
            *b"avif",
            &[exif_item(2, exif_payload(&tiff))],
            &[(2, 7)],
            true,
            1,
        );
        assert_eq!(read(&file).unwrap().exif, None);
        // A file with no `cdsc` references at all: the item is taken.
        let file = heif_with(*b"avif", &[exif_item(2, exif_payload(&tiff))], &[], true, 1);
        assert!(read(&file).unwrap().exif.is_some());
    }

    #[test]
    fn unusable_metadata_is_dropped_not_fatal() {
        // A compressed XMP packet is not inflated.
        let file = heif_with(
            *b"avif",
            &[xmp_item(3, "deflate", b"xx")],
            &[(3, 1)],
            true,
            1,
        );
        assert_eq!(read(&file).unwrap().xmp, None);
        // An Exif payload whose offset points past its end.
        let mut bad = 1000u32.to_be_bytes().to_vec();
        bad.extend_from_slice(b"II*\0");
        let file = heif_with(*b"avif", &[exif_item(2, bad)], &[(2, 1)], true, 1);
        assert_eq!(read(&file).unwrap().exif, None);
        // Not a TIFF after the offset.
        let file = heif_with(
            *b"avif",
            &[exif_item(2, vec![0, 0, 0, 0, b'n', b'o'])],
            &[(2, 1)],
            true,
            1,
        );
        assert_eq!(read(&file).unwrap().exif, None);
        // An `iloc` extent past the end of the file: the header survives.
        let mut file = heif_with(
            *b"avif",
            &[exif_item(2, exif_payload(&[0; 8]))],
            &[(2, 1)],
            false,
            0,
        );
        file.truncate(file.len() - 4);
        let h = read(&file).unwrap();
        assert_eq!((h.width, h.exif), (8, None));
    }

    #[test]
    fn nclx_points_are_read() {
        let mut colr = b"nclx".to_vec();
        colr.extend_from_slice(&[0, 12, 0, 13, 0, 6, 0x80]);
        let h = header(&heic(&[ispe(8, 8), bx(*b"colr", &colr)])).unwrap();
        assert_eq!(
            h.nclx,
            Some(Nclx {
                primaries: 12,
                transfer: 13,
                matrix: 6,
                full_range: true
            })
        );
        assert_eq!(h.icc, None);
    }
}
