//! EXIF orientation, shared by the decoders whose container carries a TIFF
//! blob: JPEG (APP1) and WebP (`EXIF` chunk). Only the orientation tag is
//! read; everything else in EXIF is stripped by policy (ADR-0001 D7).

use sqzer_core::image::Orientation;

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
    fn garbage_is_none_not_an_error() {
        assert_eq!(orientation(b"not exif at all"), None);
        assert_eq!(orientation(&tiff_with_orientation(0)), None);
        assert_eq!(orientation(&tiff_with_orientation(42)), None);
    }
}
