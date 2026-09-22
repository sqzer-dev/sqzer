//! EXIF orientation, shared by the decoders whose container carries a TIFF
//! blob: JPEG (APP1) and WebP (`EXIF` chunk). Only the orientation tag is
//! read here; the blob itself rides on the image for the metadata policy
//! (ADR-0001 D7) and the JPEG helpers below are for putting it back.

use sqzer_core::codec::Format;
use sqzer_core::image::Orientation;
use sqzer_core::{Error, Result};

/// The identifier JPEG's XMP APP1 segment starts with.
pub(crate) const XMP_APP1_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The largest payload one JPEG marker segment holds: 65535 less the two
/// length bytes.
const MARKER_MAX: usize = 65_533;

/// An XMP packet as a JPEG APP1 payload, identifier included.
///
/// # Errors
/// [`Error::Unsupported`] when the packet does not fit one marker segment:
/// Extended XMP, the multi-segment form, is not written.
pub(crate) fn xmp_app1(xmp: &[u8], format: Format) -> Result<Vec<u8>> {
    if XMP_APP1_ID.len() + xmp.len() > MARKER_MAX {
        return Err(Error::Unsupported {
            format,
            what: format!(
                "an XMP packet of {} bytes; one JPEG segment holds {}",
                xmp.len(),
                MARKER_MAX - XMP_APP1_ID.len()
            ),
        });
    }
    let mut out = XMP_APP1_ID.to_vec();
    out.extend_from_slice(xmp);
    Ok(out)
}

/// An EXIF blob as a JPEG APP1 payload, `Exif\0\0` prefix included.
/// `mozjpeg-rs` adds the prefix itself; `jpegli` takes raw markers.
///
/// # Errors
/// [`Error::Unsupported`] when the blob does not fit one marker segment.
#[cfg_attr(not(feature = "native-jpegli"), allow(dead_code))]
pub(crate) fn exif_app1(exif: &[u8], format: Format) -> Result<Vec<u8>> {
    const PREFIX: &[u8] = b"Exif\0\0";
    if PREFIX.len() + exif.len() > MARKER_MAX {
        return Err(Error::Unsupported {
            format,
            what: format!(
                "an EXIF blob of {} bytes; one JPEG segment holds {}",
                exif.len(),
                MARKER_MAX - PREFIX.len()
            ),
        });
    }
    let mut out = PREFIX.to_vec();
    out.extend_from_slice(exif);
    Ok(out)
}

/// Orientation from raw EXIF bytes starting at the TIFF header. A leading
/// `Exif\0\0` prefix is tolerated since some writers include it in
/// containers where it does not belong.
///
/// Malformed EXIF yields `None`: a photo with broken metadata still decodes,
/// just unrotated.
pub(crate) fn orientation(raw: &[u8]) -> Option<Orientation> {
    let raw = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    let parsed = exif::Reader::new().read_raw(raw.to_vec()).ok()?;
    let field = parsed.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    Orientation::from_exif(field.value.get_uint(0)?)
}

/// A minimal little-endian TIFF with a single IFD holding one
/// `Orientation` entry. Test helper for the container decoders.
#[cfg(test)]
pub(crate) fn tiff_with_orientation(value: u16) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"II");
    t.extend_from_slice(&42u16.to_le_bytes());
    t.extend_from_slice(&8u32.to_le_bytes()); // first IFD offset
    t.extend_from_slice(&1u16.to_le_bytes()); // one entry
    t.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    t.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    t.extend_from_slice(&1u32.to_le_bytes()); // count
    t.extend_from_slice(&value.to_le_bytes());
    t.extend_from_slice(&[0, 0]); // pad the 4-byte value slot
    t.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_orientation_with_or_without_prefix() {
        let tiff = tiff_with_orientation(6);
        assert_eq!(orientation(&tiff), Some(Orientation::Rotate90));
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend_from_slice(&tiff);
        assert_eq!(orientation(&prefixed), Some(Orientation::Rotate90));
    }

    #[test]
    fn app1_payloads_carry_their_identifiers_and_refuse_oversize() {
        let xmp = xmp_app1(b"<x/>", Format::Jpeg).unwrap();
        assert!(xmp.starts_with(XMP_APP1_ID) && xmp.ends_with(b"<x/>"));
        let exif = exif_app1(&tiff_with_orientation(1), Format::Jpeg).unwrap();
        assert!(exif.starts_with(b"Exif\0\0"));
        let big = vec![b'x'; 70_000];
        assert!(matches!(
            xmp_app1(&big, Format::Jpeg),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            exif_app1(&big, Format::Jpeg),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn garbage_is_none_not_an_error() {
        assert_eq!(orientation(b"not exif at all"), None);
        assert_eq!(orientation(&tiff_with_orientation(0)), None);
        assert_eq!(orientation(&tiff_with_orientation(42)), None);
    }
}
