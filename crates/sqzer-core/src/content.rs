//! A cheap guess at what kind of picture an image is, for choosing a
//! default output format (ADR-0001 D5): photographs go to a lossy codec,
//! graphics with few colours or large flat areas go to a lossless one.
//!
//! Two signals, both computed on a sample of the pixels so the cost is
//! bounded whatever the image size:
//!
//! - distinct colours: an image that uses at most 256 is a graphic, the
//!   way an indexed PNG is;
//! - flat runs: the share of horizontally adjacent pixel pairs that are
//!   identical. Screenshots, diagrams and illustrations sit well above one
//!   half; photographs and scans sit near zero even after JPEG compression.
//!
//! This is a heuristic, not a classifier. It errs toward `Photo`, because
//! a graphic sent through a lossy codec at a perceptual target still looks
//! right, while a photograph stored lossless is several times too large.

use std::collections::HashSet;

use crate::image::{Image, Samples};

/// What an image looks like, for the purpose of picking a codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Content {
    /// Continuous tone: photographs, renders, scans.
    Photo,
    /// Few colours or large flat areas: screenshots, diagrams, logos.
    Graphic,
}

/// An image with at most this many distinct colours is a graphic.
const MAX_GRAPHIC_COLORS: usize = 256;
/// An image where at least this share of adjacent pixel pairs are identical
/// is a graphic.
const FLAT_RUN_SHARE: f64 = 0.5;
/// Rows sampled for both signals. Every row is sampled below this height.
const SAMPLED_ROWS: u32 = 64;
/// Pixels visited per sampled row. Every pixel is visited below this width.
const SAMPLED_COLUMNS: u32 = 2048;

/// Classify `img`. See the module docs for what the answer means.
#[must_use]
pub fn classify(img: &Image) -> Content {
    let stats = Stats::over(img);
    if stats.colors <= MAX_GRAPHIC_COLORS || stats.flat_share() >= FLAT_RUN_SHARE {
        Content::Graphic
    } else {
        Content::Photo
    }
}

struct Stats {
    /// Distinct colours seen, saturating at one past the graphic limit.
    colors: usize,
    /// Horizontally adjacent pairs compared.
    pairs: u64,
    /// Of those, identical.
    flat: u64,
}

impl Stats {
    fn over(img: &Image) -> Self {
        let (w, h, ch) = (img.width(), img.height(), img.channels());
        let row_step = u64::from(h).div_ceil(u64::from(SAMPLED_ROWS)).max(1);
        let col_step = usize::try_from(u64::from(w).div_ceil(u64::from(SAMPLED_COLUMNS)).max(1))
            .unwrap_or(usize::MAX);
        let mut seen: HashSet<u64> = HashSet::new();
        let mut stats = Self {
            colors: 0,
            pairs: 0,
            flat: 0,
        };
        let width = w as usize;
        for y in (0..u64::from(h)).step_by(usize::try_from(row_step).unwrap_or(usize::MAX)) {
            let row_start = usize::try_from(y).unwrap_or(usize::MAX) * width * ch;
            let mut prev: Option<u64> = None;
            for x in (0..width).step_by(col_step) {
                let pixel = hash_pixel(img.samples(), row_start + x * ch, ch);
                if seen.len() <= MAX_GRAPHIC_COLORS {
                    seen.insert(pixel);
                }
                if let Some(p) = prev {
                    stats.pairs += 1;
                    if p == pixel {
                        stats.flat += 1;
                    }
                }
                prev = Some(pixel);
            }
        }
        stats.colors = seen.len();
        stats
    }

    #[allow(clippy::cast_precision_loss)]
    fn flat_share(&self) -> f64 {
        if self.pairs == 0 {
            0.0
        } else {
            self.flat as f64 / self.pairs as f64
        }
    }
}

/// The pixel at sample offset `at` folded into one integer. Equal pixels
/// fold to equal integers; distinct ones almost always to distinct ones,
/// which is all a colour count needs.
fn hash_pixel(samples: &Samples, at: usize, ch: usize) -> u64 {
    // FNV-1a over the raw sample bytes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    match samples {
        Samples::U8(v) => v[at..at + ch].iter().for_each(|&b| mix(b)),
        Samples::U16(v) => v[at..at + ch]
            .iter()
            .for_each(|s| s.to_le_bytes().into_iter().for_each(&mut mix)),
        Samples::F32(v) => v[at..at + ch]
            .iter()
            .for_each(|s| s.to_bits().to_le_bytes().into_iter().for_each(&mut mix)),
    }
    h
}

#[cfg(test)]
// Synthetic pixel data: the truncating casts are the point.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::image::ColorType;

    /// A deterministic pseudo-random image: every pixel differs from its
    /// neighbours, like sensor noise.
    fn noise(w: u32, h: u32) -> Image {
        let mut state: u32 = 0x1234_5678;
        let samples = (0..w * h * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();
        Image::from_u8(w, h, ColorType::Rgb, samples).unwrap()
    }

    /// A smooth gradient: thousands of colours, no identical neighbours.
    fn gradient(w: u32, h: u32) -> Image {
        let mut samples = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                samples.extend([
                    (x * 255 / (w - 1)) as u8,
                    (y * 255 / (h - 1)) as u8,
                    ((x + y) * 255 / (w + h - 2)) as u8,
                ]);
            }
        }
        Image::from_u8(w, h, ColorType::Rgb, samples).unwrap()
    }

    /// A gradient with a flat band across the lower half, like a
    /// screenshot with a photo in it.
    fn half_flat(w: u32, h: u32) -> Image {
        let (mut samples, _) = {
            let g = gradient(w, h);
            let parts = g.into_parts();
            (parts.3.as_u8().unwrap().to_vec(), parts)
        };
        let band = (h / 2) as usize * w as usize * 3;
        samples[band..].fill(200);
        Image::from_u8(w, h, ColorType::Rgb, samples).unwrap()
    }

    #[test]
    fn flat_and_few_colour_images_are_graphics() {
        let flat = Image::from_u8(64, 64, ColorType::Rgb, vec![7; 64 * 64 * 3]).unwrap();
        assert_eq!(classify(&flat), Content::Graphic);
        let tiny = Image::from_u8(1, 1, ColorType::Gray, vec![0]).unwrap();
        assert_eq!(classify(&tiny), Content::Graphic);
        // Sixteen-colour checkerboard: no identical neighbours, few colours.
        let samples = (0..256u32 * 256)
            .flat_map(|i| {
                let c = ((i % 256 + i / 256) % 16) as u8 * 16;
                [c, 255 - c, c / 2]
            })
            .collect();
        let checker = Image::from_u8(256, 256, ColorType::Rgb, samples).unwrap();
        assert_eq!(classify(&checker), Content::Graphic);
    }

    #[test]
    fn continuous_tone_is_a_photo() {
        assert_eq!(classify(&noise(300, 200)), Content::Photo);
        assert_eq!(classify(&gradient(300, 200)), Content::Photo);
        // The 48 x 32 test pattern of the fixtures: ramps on two axes.
        assert_eq!(classify(&gradient(48, 32)), Content::Photo);
    }

    #[test]
    fn flat_share_decides_at_one_half() {
        let img = half_flat(200, 200);
        let stats = Stats::over(&img);
        assert!(stats.colors > MAX_GRAPHIC_COLORS);
        assert!(stats.flat_share() > 0.45 && stats.flat_share() < 0.55);
        assert_eq!(classify(&img), Content::Graphic);
    }

    #[test]
    fn sampling_bounds_the_work_and_agrees_with_the_full_scan() {
        let big = noise(4000, 300);
        let stats = Stats::over(&big);
        assert!(stats.pairs <= u64::from(SAMPLED_ROWS) * u64::from(SAMPLED_COLUMNS));
        assert_eq!(classify(&big), Content::Photo);
        assert_eq!(classify(&noise(4000, 3)), Content::Photo);
    }

    #[test]
    fn every_sample_type_is_accepted() {
        let wide = Image::from_u16(8, 8, ColorType::Rgba, vec![0x1234; 8 * 8 * 4]).unwrap();
        assert_eq!(classify(&wide), Content::Graphic);
        let hdr = Image::new(8, 8, ColorType::Gray, Samples::F32(vec![0.5; 64])).unwrap();
        assert_eq!(classify(&hdr), Content::Graphic);
    }
}
