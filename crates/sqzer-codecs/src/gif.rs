//! GIF via the `gif` crate (MIT OR Apache-2.0). Decoder only: the first
//! frame, composed onto the logical screen. A file with more than one
//! frame is reported as animated; reading every frame waits for animation
//! on [`Image`]. GIF has no orientation and its ICC extension is so rare
//! that it is not read.

use std::io::Cursor;

use sqzer_core::codec::{Decoder, DecoderCaps, Format, FormatInfo, Tier};
use sqzer_core::image::{ColorType, Image};
use sqzer_core::params::DecodeOpts;
use sqzer_core::{Error, Result};

/// GIF decoder. Output is 8-bit RGB when the first frame fills the
/// screen and has no transparent index, RGBA otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct GifDecoder;

static CAPS: DecoderCaps = DecoderCaps {
    format: Format::Gif,
    name: "gif",
    animation: false,
    tier: Tier::Portable,
};

/// Header (6 bytes) plus logical screen descriptor (7 bytes).
const HEADER: usize = 13;

impl Decoder for GifDecoder {
    fn caps(&self) -> &DecoderCaps {
        &CAPS
    }

    fn probe(&self, bytes: &[u8]) -> Option<FormatInfo> {
        if bytes.len() < HEADER || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            return None;
        }
        Some(FormatInfo {
            format: Format::Gif,
            animated: frame_count(bytes, 2) > 1,
        })
    }

    fn dimensions(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        self.probe(bytes)?;
        let width = u16::from_le_bytes([bytes[6], bytes[7]]);
        let height = u16::from_le_bytes([bytes[8], bytes[9]]);
        Some((u32::from(width), u32::from(height)))
    }

    fn decode(&self, bytes: &[u8], opts: &DecodeOpts) -> Result<Image> {
        let (width, height) = self
            .dimensions(bytes)
            .ok_or_else(|| Error::Codec("not a GIF".into()))?;
        opts.check_pixels(width, height)?;
        if width == 0 || height == 0 {
            return Err(Error::Codec("GIF with an empty logical screen".into()));
        }

        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        // The pixel guard above is the limit; the crate's own default
        // (50 MB) would refuse large but legitimate files.
        options.set_memory_limit(gif::MemoryLimit::Unlimited);
        options.check_frame_consistency(true);
        let mut decoder = options.read_info(Cursor::new(bytes)).map_err(codec_err)?;
        let frame = decoder
            .read_next_frame()
            .map_err(codec_err)?
            .ok_or_else(|| Error::Codec("GIF has no image".into()))?;

        let (fw, fh) = (u32::from(frame.width), u32::from(frame.height));
        let (left, top) = (u32::from(frame.left), u32::from(frame.top));
        let full = left == 0 && top == 0 && fw == width && fh == height;
        if full && frame.transparent.is_none() {
            let rgb: Vec<u8> = frame
                .buffer
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            return Image::from_u8(width, height, ColorType::Rgb, rgb);
        }
        // Compose onto a transparent screen; a frame is clipped to it.
        let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
        let rows = fh.min(height.saturating_sub(top)) as usize;
        let cols = fw.min(width.saturating_sub(left)) as usize;
        for y in 0..rows {
            let src = y * fw as usize * 4;
            let dst = ((top as usize + y) * width as usize + left as usize) * 4;
            canvas[dst..dst + cols * 4].copy_from_slice(&frame.buffer[src..src + cols * 4]);
        }
        Image::from_u8(width, height, ColorType::Rgba, canvas)
    }
}

/// Count image descriptors by walking the block structure, stopping at
/// `stop_at`. Only block lengths are read, so it is cheap on any file.
fn frame_count(bytes: &[u8], stop_at: usize) -> usize {
    let mut pos = HEADER;
    let flags = bytes[10];
    if flags & 0x80 != 0 {
        pos += 3 << ((flags & 7) + 1);
    }
    let mut frames = 0;
    while frames < stop_at {
        match bytes.get(pos) {
            // Image descriptor: 10 bytes, optional local colour table, the
            // LZW minimum code size, then the data sub-blocks.
            Some(0x2C) => {
                frames += 1;
                let Some(&f) = bytes.get(pos + 9) else { break };
                pos += 10;
                if f & 0x80 != 0 {
                    pos += 3 << ((f & 7) + 1);
                }
                pos = skip_sub_blocks(bytes, pos + 1);
            }
            // Extension: label byte, then sub-blocks.
            Some(0x21) => pos = skip_sub_blocks(bytes, pos + 2),
            // Trailer, or something the walk does not understand.
            _ => break,
        }
    }
    frames
}

fn skip_sub_blocks(bytes: &[u8], mut pos: usize) -> usize {
    while let Some(&n) = bytes.get(pos) {
        pos += 1;
        if n == 0 {
            break;
        }
        pos += n as usize;
    }
    pos
}

fn codec_err(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal GIF: 1 x 1 screen, no global palette, `n` image blocks
    /// each carrying an empty data stream.
    fn tiny(n: usize) -> Vec<u8> {
        let mut b = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        for _ in 0..n {
            b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 0]);
        }
        b.push(0x3B);
        b
    }

    #[test]
    fn probe_counts_frames_without_decoding() {
        assert_eq!(GifDecoder.probe(&tiny(1)).map(|i| i.animated), Some(false));
        assert_eq!(GifDecoder.probe(&tiny(2)).map(|i| i.animated), Some(true));
        assert_eq!(GifDecoder.probe(b"GIF89a"), None);
        assert_eq!(GifDecoder.probe(b"RIFF....WEBPVP8 "), None);
        assert_eq!(GifDecoder.dimensions(&tiny(1)), Some((1, 1)));
    }

    #[test]
    fn a_truncated_walk_stops() {
        let mut b = tiny(1);
        b.truncate(15);
        assert_eq!(frame_count(&b, 2), 1);
    }
}
