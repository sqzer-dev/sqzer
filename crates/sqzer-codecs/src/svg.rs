//! SVG via `resvg` (Apache-2.0 OR MIT). Decoder only: the document is
//! rasterised at its own size, one CSS pixel per pixel, at 96 dpi: the
//! `width` and `height` attributes, else the `viewBox`, else the bounds
//! of what it draws. A scale comes with the resize stage. Embedded raster images (`data:`
//! URLs) are rendered, references to files on disk are refused. Text is
//! shaped with the system's fonts on desktop targets and not at all on
//! wasm32, which has no file system. Compressed `.svgz` is not read.

use std::sync::OnceLock;

use resvg::usvg;
use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// SVG rasteriser. Output is 8-bit RGBA, or RGB when nothing in the
/// document is translucent.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvgDecoder;

static CAPS: DecoderCaps = DecoderCaps {
    format: Format::Svg,
    name: "resvg",
    animation: false,
    tier: Tier::Portable,
};

/// How far into the file the sniff looks for the root element.
const SNIFF: usize = 4096;

impl Decoder for SvgDecoder {
    fn caps(&self) -> &DecoderCaps {
        &CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        root_is_svg(bytes).then_some(FormatInfo {
            format: Format::Svg,
            animated: false,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        let tree = usvg::Tree::from_data(bytes, &options(bytes)).ok()?;
        let size = tree.size().to_int_size();
        Some((size.width(), size.height()))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let tree = usvg::Tree::from_data(bytes, &options(bytes)).map_err(codec_err)?;
        let size = tree.size().to_int_size();
        let (width, height) = (size.width(), size.height());
        opts.check_pixels(width, height)?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| Error::Codec("SVG size is empty or too large".into()))?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        let pixels = pixmap.pixels();
        if pixels.iter().all(|p| p.alpha() == u8::MAX) {
            let rgb: Vec<u8> = pixels
                .iter()
                .flat_map(|p| [p.red(), p.green(), p.blue()])
                .collect();
            return Image::from_u8(width, height, ColorType::Rgb, rgb);
        }
        let rgba: Vec<u8> = pixels
            .iter()
            .map(resvg::tiny_skia::PremultipliedColorU8::demultiply)
            .flat_map(|p| [p.red(), p.green(), p.blue(), p.alpha()])
            .collect();
        Image::from_u8(width, height, ColorType::Rgba, rgba)
    }
}

/// Parse options: no file access for `<image>` references, and fonts only
/// when the document has text, since loading the system's takes longer
/// than rendering most files.
fn options(bytes: &[u8]) -> usvg::Options<'static> {
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    if bytes.windows(5).any(|w| w == b"<text") {
        options.fontdb = fonts().clone();
    }
    options
}

/// The system's fonts, loaded once per process. Empty on wasm32.
fn fonts() -> &'static std::sync::Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<std::sync::Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| std::sync::Arc::new(system_fonts()))
}

#[cfg(not(target_arch = "wasm32"))]
fn system_fonts() -> usvg::fontdb::Database {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    db
}

#[cfg(target_arch = "wasm32")]
fn system_fonts() -> usvg::fontdb::Database {
    usvg::fontdb::Database::new()
}

/// Whether the first element of the document is `<svg`, after an optional
/// byte-order mark, XML declaration, comments and a doctype.
fn root_is_svg(bytes: &[u8]) -> bool {
    let mut s = &bytes[..bytes.len().min(SNIFF)];
    if let Some(rest) = s.strip_prefix(b"\xEF\xBB\xBF") {
        s = rest;
    }
    loop {
        s = skip_ws(s);
        let skipped = if s.starts_with(b"<?") {
            find(s, b"?>")
        } else if s.starts_with(b"<!--") {
            find(s, b"-->")
        } else if s.starts_with(b"<!") {
            find(s, b">")
        } else {
            break;
        };
        match skipped {
            Some(end) => s = &s[end..],
            None => return false,
        }
    }
    s.strip_prefix(b"<svg")
        .and_then(|rest| rest.first())
        .is_some_and(|c| c.is_ascii_whitespace() || matches!(c, b'>' | b'/'))
}

fn skip_ws(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|c| c.is_ascii_whitespace()).count();
    &s[n..]
}

/// Index just past the first occurrence of `needle`.
fn find(s: &[u8], needle: &[u8]) -> Option<usize> {
    s.windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + needle.len())
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_finds_the_root_element() {
        assert!(root_is_svg(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(root_is_svg(
            b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<!-- c -->\n<!DOCTYPE svg>\n<svg>"
        ));
        assert!(root_is_svg(b"  <svg\n"));
        assert!(!root_is_svg(b"<!DOCTYPE html><html><svg/></html>"));
        assert!(!root_is_svg(b"<svgfoo/>"));
        assert!(!root_is_svg(b"<?xml version=\"1.0\"?>"));
        assert!(!root_is_svg(b"P6\n"));
        assert!(!root_is_svg(b""));
    }

    #[test]
    fn a_document_without_width_and_height_takes_its_view_box() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 5\"><rect width=\"1\" height=\"1\"/></svg>";
        assert_eq!(SvgDecoder.dimensions(svg), Some((10, 5)));
        let img = SvgDecoder.decode(svg, &DecodeOpts::default()).unwrap();
        assert_eq!((img.width(), img.height()), (10, 5));
        assert_eq!(img.color(), ColorType::Rgba);
    }

    #[test]
    fn file_references_are_refused() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"2\" height=\"2\"><image xlink:href=\"/etc/hostname\" width=\"2\" height=\"2\"/></svg>";
        let img = SvgDecoder.decode(svg, &DecodeOpts::default()).unwrap();
        assert_eq!(img.color(), ColorType::Rgba);
        assert!(img.samples().as_u8().unwrap().iter().all(|&b| b == 0));
    }
}
