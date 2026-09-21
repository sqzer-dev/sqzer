//! The resize stage of ADR-0001 D3, an adapter over `fast_image_resize`.
//!
//! The geometry is [`Resize::fit`]; this module only moves samples. Integer
//! samples are taken to be sRGB-encoded, so they are mapped to 16-bit
//! linear light, resampled there and mapped back: averaging encoded values
//! darkens fine detail, which is what Squoosh's "linear RGB" default
//! avoids. Float samples are linear already. Alpha is premultiplied for the
//! resampling and divided out after, so a transparent pixel's colour does
//! not bleed into its neighbours. The filter is Lanczos3.
//!
//! > **Note**: an image that keeps a non-sRGB ICC profile is still
//! > linearised with the sRGB curve. For Display P3 that is exact; for a
//! > profile with another curve it is an approximation, and a closer one
//! > than resampling the encoded values.

use fast_image_resize::images::{Image as Buffer, ImageRef};
use fast_image_resize::{PixelType, ResizeOptions, Resizer, create_srgb_mapper};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::params::Resize;
use sqzer_core::{Error, Result};

/// `image` scaled to fit `bounds`, or `image` itself when it already does.
/// The ICC profile stays on the image.
pub fn fit(image: Image, bounds: Resize) -> Result<Image> {
    let Some((width, height)) = bounds.fit(image.width(), image.height()) else {
        return Ok(image);
    };
    let (src_width, src_height, color, samples, icc) = image.into_parts();
    let from = (src_width, src_height);
    let to = (width, height);
    let samples = match samples {
        Samples::U8(v) => Samples::U8(through_linear(&v, from, to, color, pixel_u8(color))?),
        Samples::U16(v) => Samples::U16(through_linear(&v, from, to, color, pixel_u16(color))?),
        Samples::F32(v) => {
            let mut out = resample(&v, from, to, pixel_f32(color))?;
            // Lanczos rings; linear light has no negative values.
            for s in &mut out {
                *s = s.max(0.0);
            }
            Samples::F32(out)
        }
    };
    Ok(Image::new(width, height, color, samples)?.with_icc(icc))
}

/// sRGB-encoded samples to 16-bit linear, resampled, and back to `T`.
fn through_linear<T: bytemuck::Pod>(
    samples: &[T],
    from: (u32, u32),
    to: (u32, u32),
    color: ColorType,
    encoded: PixelType,
) -> Result<Vec<T>> {
    let linear = pixel_u16(color);
    let mapper = create_srgb_mapper();

    let mut wide = vec![0u16; samples.len()];
    {
        let src = view(samples, from, encoded)?;
        let mut dst = view_mut(&mut wide, from, linear)?;
        mapper.forward_map(&src, &mut dst).map_err(failed)?;
    }
    let small = resample(&wide, from, to, linear)?;
    drop(wide);

    let mut out = vec![T::zeroed(); small.len()];
    {
        let src = view(&small, to, linear)?;
        let mut dst = view_mut(&mut out, to, encoded)?;
        mapper.backward_map(&src, &mut dst).map_err(failed)?;
    }
    Ok(out)
}

/// One Lanczos3 pass with premultiplied alpha, in the samples' own type.
fn resample<T: bytemuck::Pod>(
    samples: &[T],
    from: (u32, u32),
    to: (u32, u32),
    pixel: PixelType,
) -> Result<Vec<T>> {
    let channels = pixel.size() / size_of::<T>();
    let mut out = vec![T::zeroed(); to.0 as usize * to.1 as usize * channels];
    let src = view(samples, from, pixel)?;
    let mut dst = view_mut(&mut out, to, pixel)?;
    // The defaults are the policy: Lanczos3 convolution, alpha multiplied
    // in and divided out around it.
    Resizer::new()
        .resize(&src, &mut dst, &ResizeOptions::new())
        .map_err(failed)?;
    Ok(out)
}

// `fast_image_resize` takes bytes and checks their alignment. Casting from
// a typed slice keeps the alignment right by construction.
fn view<T: bytemuck::Pod>(
    samples: &[T],
    (width, height): (u32, u32),
    pixel: PixelType,
) -> Result<ImageRef<'_>> {
    ImageRef::new(width, height, bytemuck::cast_slice(samples), pixel).map_err(failed)
}

fn view_mut<T: bytemuck::Pod>(
    samples: &mut [T],
    (width, height): (u32, u32),
    pixel: PixelType,
) -> Result<Buffer<'_>> {
    Buffer::from_slice_u8(width, height, bytemuck::cast_slice_mut(samples), pixel).map_err(failed)
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Transform {
        stage: "resize",
        message: e.to_string(),
    }
}

const fn pixel_u8(color: ColorType) -> PixelType {
    match color {
        ColorType::Gray => PixelType::U8,
        ColorType::GrayAlpha => PixelType::U8x2,
        ColorType::Rgb => PixelType::U8x3,
        ColorType::Rgba => PixelType::U8x4,
    }
}

const fn pixel_u16(color: ColorType) -> PixelType {
    match color {
        ColorType::Gray => PixelType::U16,
        ColorType::GrayAlpha => PixelType::U16x2,
        ColorType::Rgb => PixelType::U16x3,
        ColorType::Rgba => PixelType::U16x4,
    }
}

const fn pixel_f32(color: ColorType) -> PixelType {
    match color {
        ColorType::Gray => PixelType::F32,
        ColorType::GrayAlpha => PixelType::F32x2,
        ColorType::Rgb => PixelType::F32x3,
        ColorType::Rgba => PixelType::F32x4,
    }
}

#[cfg(test)]
// Synthetic pixel data: the truncating casts are the point.
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    const ALL: [ColorType; 4] = [
        ColorType::Gray,
        ColorType::GrayAlpha,
        ColorType::Rgb,
        ColorType::Rgba,
    ];

    fn width(w: u32) -> Resize {
        Resize {
            max_width: Some(w),
            max_height: None,
        }
    }

    /// Every sample of every pixel set to `value`, alpha included.
    fn flat_u8(w: u32, h: u32, color: ColorType, value: u8) -> Image {
        let n = w as usize * h as usize * color.channels();
        Image::from_u8(w, h, color, vec![value; n]).unwrap()
    }

    #[test]
    fn an_image_inside_the_bounds_comes_back_untouched() {
        let img = flat_u8(40, 30, ColorType::Rgb, 90).with_icc(Some(vec![1, 2, 3]));
        assert_eq!(fit(img.clone(), width(40)).unwrap(), img);
        assert_eq!(fit(img.clone(), width(4000)).unwrap(), img);
        assert_eq!(fit(img.clone(), Resize::NONE).unwrap(), img);
    }

    #[test]
    fn every_layout_and_depth_keeps_its_type_and_aspect() {
        for color in ALL {
            let n = 64 * 48 * color.channels();
            let cases = [
                Samples::U8(vec![200; n]),
                Samples::U16(vec![51_400; n]),
                Samples::F32(vec![0.5; n]),
            ];
            for samples in cases {
                let format = samples.format();
                let img = Image::new(64, 48, color, samples).unwrap();
                let out = fit(img, width(16)).unwrap();
                assert_eq!(
                    (out.width(), out.height()),
                    (16, 12),
                    "{color:?} {format:?}"
                );
                assert_eq!(out.color(), color);
                assert_eq!(out.sample_format(), format);
                // A flat image stays flat through the linear round trip
                // and the alpha multiply and divide.
                let flat = match out.samples() {
                    Samples::U8(v) => v.iter().all(|&s| s == 200),
                    Samples::U16(v) => v.iter().all(|&s| s.abs_diff(51_400) <= 1),
                    Samples::F32(v) => v.iter().all(|&s| (s - 0.5).abs() < 1e-4),
                };
                assert!(flat, "{color:?} {:?}", out.samples());
            }
        }
    }

    #[test]
    fn sixteen_bit_input_keeps_sixteen_bit_precision() {
        // A horizontal ramp with steps far below one 8-bit level.
        let samples: Vec<u16> = (0..32u32)
            .flat_map(|_| (0..256u32).map(|x| 30_000 + x as u16 * 4))
            .collect();
        let img = Image::from_u16(256, 32, ColorType::Gray, samples).unwrap();
        let out = fit(img, width(64)).unwrap();
        let row = &out.samples().as_u16().unwrap()[..64];
        // Away from the edges the ramp is still strictly increasing: the
        // 16 source units between output pixels survived.
        assert!(row[4..60].windows(2).all(|w| w[1] > w[0]), "{row:?}");
    }

    #[test]
    fn the_icc_profile_stays_on_the_image() {
        let img = flat_u8(8, 8, ColorType::Rgb, 10).with_icc(Some(vec![7; 16]));
        let out = fit(img, width(4)).unwrap();
        assert_eq!(out.icc(), Some(&[7u8; 16][..]));
    }

    #[test]
    fn averaging_happens_in_linear_light() {
        // One-pixel black and white stripes, halved. The mean of 0 and 255
        // in encoded values is 128; in linear light it is 0.5, which
        // encodes to 188.
        let samples: Vec<u8> = (0..64 * 64)
            .map(|i| if i % 2 == 0 { 0 } else { 255 })
            .collect();
        let img = Image::from_u8(64, 64, ColorType::Gray, samples).unwrap();
        let out = fit(img, width(32)).unwrap();
        let v = out.samples().as_u8().unwrap();
        let middle = v[16 * 32 + 16];
        assert!((180..=196).contains(&middle), "{middle}");
    }

    #[test]
    fn transparent_pixels_do_not_bleed_their_colour() {
        // Left half opaque red, right half fully transparent green.
        let mut samples = Vec::new();
        for _ in 0..32 {
            for x in 0..32 {
                samples.extend_from_slice(if x < 16 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 255, 0, 0]
                });
            }
        }
        let img = Image::from_u8(32, 32, ColorType::Rgba, samples).unwrap();
        let out = fit(img, width(8)).unwrap();
        let v = out.samples().as_u8().unwrap();
        for px in v.as_chunks::<4>().0.iter().filter(|px| px[3] > 8) {
            assert!(px[0] > 240 && px[1] < 16, "green bled in: {px:?}");
        }
    }

    #[test]
    fn float_samples_stay_linear_and_non_negative() {
        // A hard edge from 0 to 4.0: Lanczos undershoots next to it.
        let samples: Vec<f32> = (0..64 * 8)
            .map(|i| if i % 64 < 32 { 0.0 } else { 4.0 })
            .collect();
        let img = Image::new(64, 8, ColorType::Gray, Samples::F32(samples)).unwrap();
        let out = fit(img, width(16)).unwrap();
        let v = out.samples().as_f32().unwrap();
        assert!(v.iter().all(|&s| s >= 0.0), "{v:?}");
        // Values above one are kept: this is HDR, not a clamp to display.
        assert!(v.iter().any(|&s| s > 3.5), "{v:?}");
    }
}
