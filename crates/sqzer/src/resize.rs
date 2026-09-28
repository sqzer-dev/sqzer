//! The resize stage of ADR-0009, an adapter over `fast_image_resize`.
//!
//! The geometry is [`Resize::fit`](sqzer_core::resize::Resize::fit); this
//! module only moves samples. Integer samples are taken to be
//! sRGB-encoded, so they are mapped to 16-bit linear light, resampled
//! there and mapped back: averaging encoded values darkens fine detail,
//! which is what Squoosh's "linear RGB" default avoids. Float samples are
//! linear already. Alpha is premultiplied for the resampling and divided
//! out after, so a transparent pixel's colour does not bleed into its
//! neighbours. The filter is Lanczos3 unless the request names another.
//! [`Filter::Nearest`] copies source pixels as they are, in their own
//! encoding and without the alpha multiply, so pixel art keeps its exact
//! colours.
//!
//! The crop of a cover fit is part of the same pass. The padding of a
//! contain fit is [`pad`], run by the encode stage.
//!
//! > **Note**: the colour stage runs first, so integer samples are sRGB
//! > here unless `keep_icc` left a profile on the image. Such an image is
//! > still linearised with the sRGB curve: exact for Display P3, an
//! > approximation for a profile with another curve, and a closer one
//! > than resampling the encoded values. The same holds for the padding
//! > colour, which is given in sRGB.

use fast_image_resize::images::{Image as Buffer, ImageRef};
use fast_image_resize::{
    FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, create_srgb_mapper,
};
use sqzer_core::image::{ColorType, Image, Samples};
use sqzer_core::resize::{Canvas, Filter, Geometry};
use sqzer_core::{Error, Result};

/// `image` cropped and resampled as `geometry` says. The canvas is left
/// for [`pad`]. The ICC profile and the metadata stay on the image.
pub fn resample(image: &Image, geometry: &Geometry, filter: Filter) -> Result<Image> {
    let color = image.color();
    let from = (image.width(), image.height());
    let to = (geometry.width, geometry.height);
    let options = options(geometry, filter);
    let samples = match image.samples() {
        Samples::U8(v) if filter == Filter::Nearest => {
            Samples::U8(resample_in(v, from, to, pixel_u8(color), options)?)
        }
        Samples::U16(v) if filter == Filter::Nearest => {
            Samples::U16(resample_in(v, from, to, pixel_u16(color), options)?)
        }
        Samples::U8(v) => Samples::U8(through_linear(
            v,
            from,
            to,
            color,
            pixel_u8(color),
            options,
        )?),
        Samples::U16(v) => Samples::U16(through_linear(
            v,
            from,
            to,
            color,
            pixel_u16(color),
            options,
        )?),
        Samples::F32(v) => {
            let mut out = resample_in(v, from, to, pixel_f32(color), options)?;
            // Lanczos rings; linear light has no negative values.
            for s in &mut out {
                *s = s.max(0.0);
            }
            Samples::F32(out)
        }
    };
    Ok(Image::new(to.0, to.1, color, samples)?.with_metadata(image.metadata().clone()))
}

/// The resampler's options for `geometry` and `filter`.
fn options(geometry: &Geometry, filter: Filter) -> ResizeOptions {
    let convolution = |f| ResizeAlg::Convolution(f);
    let algorithm = match filter {
        Filter::Lanczos3 => convolution(FilterType::Lanczos3),
        Filter::Mitchell => convolution(FilterType::Mitchell),
        Filter::CatmullRom => convolution(FilterType::CatmullRom),
        Filter::Bilinear => convolution(FilterType::Bilinear),
        Filter::Box => convolution(FilterType::Box),
        Filter::Nearest => ResizeAlg::Nearest,
    };
    let mut options = ResizeOptions::new()
        .resize_alg(algorithm)
        .use_alpha(filter != Filter::Nearest);
    if let Some(c) = geometry.crop {
        options = options.crop(c.left, c.top, c.width, c.height);
    }
    options
}

/// `image` placed on `canvas`. `alpha` says whether the encoder takes an
/// alpha channel: it decides the default background, transparent or
/// white, and without it the background is made opaque. The layout grows
/// to hold the background: a grey image on a coloured background becomes
/// RGB, an opaque one on a translucent background gains alpha.
///
/// # Errors
/// [`Error::Transform`] when the image does not fit the canvas at its
/// offset.
pub fn pad(image: &Image, canvas: &Canvas, alpha: bool) -> Result<Image> {
    let fits = |at: u32, side: u32, of: u32| u64::from(at) + u64::from(side) <= u64::from(of);
    if !fits(canvas.x, image.width(), canvas.width)
        || !fits(canvas.y, image.height(), canvas.height)
    {
        return Err(Error::Transform {
            stage: "pad",
            message: format!(
                "a {}x{} image does not fit a {}x{} canvas at {},{}",
                image.width(),
                image.height(),
                canvas.width,
                canvas.height,
                canvas.x,
                canvas.y
            ),
        });
    }
    let mut bg = canvas
        .background
        .unwrap_or(if alpha { [0, 0, 0, 0] } else { [255; 4] });
    if !alpha {
        bg[3] = 255;
    }
    let src = image.color();
    let grey =
        matches!(src, ColorType::Gray | ColorType::GrayAlpha) && bg[0] == bg[1] && bg[1] == bg[2];
    let color = match (grey, src.has_alpha() || bg[3] < 255) {
        (true, false) => ColorType::Gray,
        (true, true) => ColorType::GrayAlpha,
        (false, false) => ColorType::Rgb,
        (false, true) => ColorType::Rgba,
    };
    let layout = Layout {
        src,
        dst: color,
        size: (image.width(), image.height()),
        canvas,
    };
    let samples = match image.samples() {
        Samples::U8(v) => Samples::U8(layout.place(v, bg, u8::MAX)),
        Samples::U16(v) => Samples::U16(layout.place(v, bg.map(|c| u16::from(c) * 257), u16::MAX)),
        Samples::F32(v) => {
            let unit = bg.map(|c| f32::from(c) / 255.0);
            let mut linear = unit.map(srgb_to_linear);
            // Alpha is a fraction, not an encoded value.
            linear[3] = unit[3];
            Samples::F32(layout.place(v, linear, 1.0))
        }
    };
    Ok(Image::new(canvas.width, canvas.height, color, samples)?
        .with_metadata(image.metadata().clone()))
}

/// Where the samples of one image go on a canvas, and in what layout.
struct Layout<'a> {
    src: ColorType,
    dst: ColorType,
    size: (u32, u32),
    canvas: &'a Canvas,
}

impl Layout<'_> {
    /// The canvas filled with `bg`, RGBA, and the source copied in at its
    /// offset. Grey is spread to RGB, missing alpha is `opaque`.
    fn place<T: Copy>(&self, src: &[T], bg: [T; 4], opaque: T) -> Vec<T> {
        let fill: &[T] = match self.dst {
            ColorType::Gray => &bg[..1],
            ColorType::GrayAlpha => &[bg[0], bg[3]],
            ColorType::Rgb => &bg[..3],
            ColorType::Rgba => &bg,
        };
        let (sc, dc) = (self.src.channels(), self.dst.channels());
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let cw = self.canvas.width as usize;
        let (cx, cy) = (self.canvas.x as usize, self.canvas.y as usize);
        let mut out = fill.repeat(cw * self.canvas.height as usize);
        for y in 0..h {
            let from = &src[y * w * sc..][..w * sc];
            let to = &mut out[((cy + y) * cw + cx) * dc..][..w * dc];
            if self.src == self.dst {
                to.copy_from_slice(from);
                continue;
            }
            for (s, d) in from.chunks_exact(sc).zip(to.chunks_exact_mut(dc)) {
                let grey_src = matches!(self.src, ColorType::Gray | ColorType::GrayAlpha);
                let colour = if grey_src {
                    [s[0]; 3]
                } else {
                    [s[0], s[1], s[2]]
                };
                let alpha = if self.src.has_alpha() {
                    s[sc - 1]
                } else {
                    opaque
                };
                match self.dst {
                    ColorType::Gray => d[0] = colour[0],
                    ColorType::GrayAlpha => {
                        d[0] = colour[0];
                        d[1] = alpha;
                    }
                    ColorType::Rgb => d.copy_from_slice(&colour),
                    ColorType::Rgba => {
                        d[..3].copy_from_slice(&colour);
                        d[3] = alpha;
                    }
                }
            }
        }
        out
    }
}

/// The sRGB transfer curve to linear light, both on `0.0..=1.0`.
fn srgb_to_linear(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB-encoded samples to 16-bit linear, resampled, and back to `T`.
fn through_linear<T: bytemuck::Pod>(
    samples: &[T],
    from: (u32, u32),
    to: (u32, u32),
    color: ColorType,
    encoded: PixelType,
    options: ResizeOptions,
) -> Result<Vec<T>> {
    let linear = pixel_u16(color);
    let mapper = create_srgb_mapper();

    let mut wide = vec![0u16; samples.len()];
    {
        let src = view(samples, from, encoded)?;
        let mut dst = view_mut(&mut wide, from, linear)?;
        mapper.forward_map(&src, &mut dst).map_err(failed)?;
    }
    let small = resample_in(&wide, from, to, linear, options)?;
    drop(wide);

    let mut out = vec![T::zeroed(); small.len()];
    {
        let src = view(&small, to, linear)?;
        let mut dst = view_mut(&mut out, to, encoded)?;
        mapper.backward_map(&src, &mut dst).map_err(failed)?;
    }
    Ok(out)
}

/// One pass of the resampler, in the samples' own type.
fn resample_in<T: bytemuck::Pod>(
    samples: &[T],
    from: (u32, u32),
    to: (u32, u32),
    pixel: PixelType,
    options: ResizeOptions,
) -> Result<Vec<T>> {
    let channels = pixel.size() / size_of::<T>();
    let mut out = vec![T::zeroed(); to.0 as usize * to.1 as usize * channels];
    let src = view(samples, from, pixel)?;
    let mut dst = view_mut(&mut out, to, pixel)?;
    Resizer::new()
        .resize(&src, &mut dst, &options)
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
    use sqzer_core::resize::{Fit, Position, Resize, Size};

    const ALL: [ColorType; 4] = [
        ColorType::Gray,
        ColorType::GrayAlpha,
        ColorType::Rgb,
        ColorType::Rgba,
    ];

    fn width(w: u32) -> Resize {
        Resize::inside(Some(w), None)
    }

    /// The resize stage without the padding, as the facade runs it.
    fn fit(image: Image, r: Resize) -> Result<Image> {
        match r.fit(image.width(), image.height()) {
            Some(g) if g.resamples(image.width(), image.height()) => resample(&image, &g, r.filter),
            _ => Ok(image),
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

    /// A 4 x 2 image whose left half is one value and right half another.
    fn halves(left: u8, right: u8) -> Image {
        Image::from_u8(
            4,
            2,
            ColorType::Gray,
            vec![left, left, right, right, left, left, right, right],
        )
        .unwrap()
    }

    fn boxed(w: u32, h: u32, fit: Fit, position: Position) -> Resize {
        Resize {
            size: Size::Box {
                width: Some(w),
                height: Some(h),
            },
            fit,
            position,
            ..Resize::NONE
        }
    }

    #[test]
    fn cover_keeps_the_part_at_the_position() {
        // 4 x 2 into 2 x 2 without enlarging: a 2 x 2 crop, copied as is.
        let img = halves(10, 250);
        let left = fit(img.clone(), boxed(2, 2, Fit::Cover, Position::Left)).unwrap();
        assert_eq!(left.samples().as_u8().unwrap(), &[10; 4]);
        let right = fit(img.clone(), boxed(2, 2, Fit::Cover, Position::Right)).unwrap();
        assert_eq!(right.samples().as_u8().unwrap(), &[250; 4]);
        // Scaled down as well: 4 x 2 into 1 x 1 keeps the right 2 x 2
        // square. The filter reads its support past the crop edge, as it
        // would anywhere else in the picture, so a little of the left half
        // shows.
        let one = fit(img, boxed(1, 1, Fit::Cover, Position::Right)).unwrap();
        let v = one.samples().as_u8().unwrap()[0];
        assert!((235..=250).contains(&v), "{v}");
    }

    #[test]
    fn nearest_keeps_exact_colours() {
        // A checkerboard of two colours, a quarter translucent.
        let mut samples = Vec::new();
        for y in 0..16u32 {
            for x in 0..16u32 {
                samples.extend_from_slice(if (x / 2 + y / 2) % 2 == 0 {
                    &[200, 30, 90, 255]
                } else {
                    &[10, 180, 60, 64]
                });
            }
        }
        let img = Image::from_u8(16, 16, ColorType::Rgba, samples).unwrap();
        let r = Resize {
            filter: Filter::Nearest,
            ..width(8)
        };
        let out = fit(img, r).unwrap();
        for px in out.samples().as_u8().unwrap().as_chunks::<4>().0 {
            assert!(
                *px == [200, 30, 90, 255] || *px == [10, 180, 60, 64],
                "a new colour: {px:?}"
            );
        }
    }

    #[test]
    fn every_filter_runs() {
        for filter in [
            Filter::Lanczos3,
            Filter::Mitchell,
            Filter::CatmullRom,
            Filter::Bilinear,
            Filter::Box,
            Filter::Nearest,
        ] {
            let r = Resize { filter, ..width(3) };
            let out = fit(flat_u8(16, 16, ColorType::Rgba, 120), r).unwrap();
            assert_eq!((out.width(), out.height()), (3, 3), "{filter:?}");
            assert!(
                out.samples().as_u8().unwrap().iter().all(|&s| s == 120),
                "{filter:?}"
            );
        }
    }

    fn canvas(w: u32, h: u32, x: u32, y: u32, background: Option<[u8; 4]>) -> Canvas {
        Canvas {
            width: w,
            height: h,
            x,
            y,
            background,
        }
    }

    #[test]
    fn pad_places_the_image_on_the_default_background() {
        let img = flat_u8(2, 1, ColorType::Rgb, 90);
        // An encoder with alpha: transparent padding, and the image gains
        // an opaque alpha channel.
        let out = pad(&img, &canvas(4, 1, 1, 0, None), true).unwrap();
        assert_eq!(out.color(), ColorType::Rgba);
        assert_eq!(
            out.samples().as_u8().unwrap(),
            &[0, 0, 0, 0, 90, 90, 90, 255, 90, 90, 90, 255, 0, 0, 0, 0]
        );
        // Without alpha: white, and the layout stays.
        let out = pad(&img, &canvas(2, 2, 0, 1, None), false).unwrap();
        assert_eq!(out.color(), ColorType::Rgb);
        assert_eq!(
            out.samples().as_u8().unwrap(),
            &[255, 255, 255, 255, 255, 255, 90, 90, 90, 90, 90, 90]
        );
        // A translucent background given for an encoder without alpha is
        // made opaque.
        let out = pad(&img, &canvas(3, 1, 0, 0, Some([0, 0, 255, 0])), false).unwrap();
        assert_eq!(out.color(), ColorType::Rgb);
        assert_eq!(&out.samples().as_u8().unwrap()[6..], &[0, 0, 255]);
    }

    #[test]
    fn pad_grows_the_layout_to_hold_the_background() {
        let grey = flat_u8(1, 1, ColorType::Gray, 7);
        // Grey on grey stays grey.
        let out = pad(&grey, &canvas(2, 1, 0, 0, Some([30, 30, 30, 255])), true).unwrap();
        assert_eq!(out.color(), ColorType::Gray);
        assert_eq!(out.samples().as_u8().unwrap(), &[7, 30]);
        // Grey on red becomes RGB.
        let out = pad(&grey, &canvas(2, 1, 1, 0, Some([255, 0, 0, 255])), true).unwrap();
        assert_eq!(out.color(), ColorType::Rgb);
        assert_eq!(out.samples().as_u8().unwrap(), &[255, 0, 0, 7, 7, 7]);
        // Grey with alpha on red keeps its alpha.
        let ga = Image::from_u8(1, 1, ColorType::GrayAlpha, vec![7, 99]).unwrap();
        let out = pad(&ga, &canvas(2, 1, 0, 0, Some([255, 0, 0, 255])), true).unwrap();
        assert_eq!(out.color(), ColorType::Rgba);
        assert_eq!(
            out.samples().as_u8().unwrap(),
            &[7, 7, 7, 99, 255, 0, 0, 255]
        );
    }

    #[test]
    fn pad_works_in_every_layout_and_sample_width() {
        for color in ALL {
            let n = 2 * 2 * color.channels();
            for samples in [
                Samples::U8(vec![100; n]),
                Samples::U16(vec![25_700; n]),
                Samples::F32(vec![0.5; n]),
            ] {
                let format = samples.format();
                let img = Image::new(2, 2, color, samples)
                    .unwrap()
                    .with_icc(Some(vec![1, 2, 3]));
                for alpha in [false, true] {
                    let out = pad(&img, &canvas(4, 3, 1, 1, None), alpha).unwrap();
                    assert_eq!((out.width(), out.height()), (4, 3), "{color:?} {format:?}");
                    assert_eq!(out.sample_format(), format);
                    assert_eq!(out.has_alpha(), alpha || color.has_alpha());
                    assert_eq!(out.icc(), Some(&[1u8, 2, 3][..]));
                    // The corner is background: white when opaque,
                    // transparent otherwise.
                    let ch = out.channels();
                    let corner_alpha_zero = match out.samples() {
                        Samples::U8(v) => v[..ch].iter().all(|&s| s == 0),
                        Samples::U16(v) => v[..ch].iter().all(|&s| s == 0),
                        Samples::F32(v) => v[..ch].iter().all(|&s| s == 0.0),
                    };
                    let corner_white = match out.samples() {
                        Samples::U8(v) => v[..ch].iter().all(|&s| s == u8::MAX),
                        Samples::U16(v) => v[..ch].iter().all(|&s| s == u16::MAX),
                        Samples::F32(v) => v[..ch].iter().all(|&s| (s - 1.0).abs() < 1e-6),
                    };
                    assert!(
                        if alpha {
                            corner_alpha_zero
                        } else {
                            corner_white
                        },
                        "{color:?} {format:?} alpha {alpha}: {:?}",
                        out.samples()
                    );
                }
            }
        }
    }

    #[test]
    fn pad_refuses_an_image_larger_than_the_canvas() {
        let img = flat_u8(3, 3, ColorType::Rgb, 1);
        let err = pad(&img, &canvas(4, 4, 2, 0, None), true).unwrap_err();
        assert!(
            matches!(err, Error::Transform { stage: "pad", .. }),
            "{err}"
        );
    }
}
