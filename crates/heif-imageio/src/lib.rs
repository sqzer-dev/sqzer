//! HEIC decoding through macOS `ImageIO`, for `sqzer`'s `native-heif`
//! feature (ADR-0005 D2). Every Mac since 10.13 decodes HEIC in the OS;
//! `available` checks that this one does by asking `ImageIO` whether it
//! lists `public.heic`, once per process, and `decode` draws the
//! primary image into a Quartz bitmap context of a known layout.
//!
//! The context uses the image's own colour space, so no colour conversion
//! happens; an 8-bit source lands in an 8-bit context, anything deeper in
//! a 16-bit one. Quartz has no non-premultiplied RGB context, so an image
//! with alpha comes back premultiplied and says so. The container side
//! (brands, size, rotation, ICC) is read in safe Rust by `sqzer-codecs`,
//! which also applies the rotation: `CGImageSourceCreateImageAtIndex`
//! does not apply `irot`, but it does apply `clap`.
//!
//! Empty on every OS but macOS.

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::OnceLock;

    use objc2_core_foundation::{CFData, CFString, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGColorSpaceModel, CGContext, CGImage,
        CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_image_io::CGImageSource;

    /// Interleaved, tightly packed samples from one decode.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Samples {
        /// 8 bits per sample.
        U8(Vec<u8>),
        /// 16 bits per sample, full range.
        U16(Vec<u16>),
    }

    /// One decoded image: `width * height * channels` samples, row-major,
    /// in the container's stored orientation, `clap` applied.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Raw {
        /// Width in pixels.
        pub width: u32,
        /// Height in pixels.
        pub height: u32,
        /// 1 gray, 3 RGB, 4 RGBA.
        pub channels: u8,
        /// The samples.
        pub samples: Samples,
        /// Whether the colour samples are premultiplied by alpha. True
        /// for every image with alpha: Quartz draws that way.
        pub premultiplied: bool,
    }

    /// Why a decode did not happen.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Error {
        /// `ImageIO` on this macOS has no HEIC decoder. The string is the
        /// reason [`available`] gives.
        Unavailable(String),
        /// The image is larger than the caller allows.
        TooLarge {
            /// Pixels in the image.
            pixels: u64,
            /// The caller's limit.
            limit: u64,
        },
        /// `ImageIO` or Quartz refused the file.
        Decode(String),
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Unavailable(reason) | Self::Decode(reason) => f.write_str(reason),
                Self::TooLarge { pixels, limit } => {
                    write!(f, "image has {pixels} pixels, limit is {limit}")
                }
            }
        }
    }

    impl std::error::Error for Error {}

    /// Whether `ImageIO` lists `public.heic` among the types it reads.
    /// Built into macOS 10.13 and later; Rust's Intel floor is 10.12, so
    /// this is asked once rather than assumed.
    ///
    /// # Errors
    /// The reason, phrased for the user.
    pub fn available() -> Result<(), String> {
        static PROBED: OnceLock<Result<(), String>> = OnceLock::new();
        PROBED
            .get_or_init(|| {
                // SAFETY: no preconditions; the array is retained and
                // holds CFStrings, per the ImageIO documentation.
                let identifiers = unsafe { CGImageSource::type_identifiers() };
                let count = isize::try_from(identifiers.len()).unwrap_or(0);
                for index in 0..count {
                    // SAFETY: `index` is in bounds and every element is a
                    // CFString the array keeps alive.
                    let string = unsafe { identifiers.value_at_index(index) }.cast::<CFString>();
                    if string.is_null() {
                        continue;
                    }
                    // SAFETY: as above.
                    if unsafe { &*string }.to_string() == "public.heic" {
                        return Ok(());
                    }
                }
                Err("ImageIO on this macOS cannot read HEIC; macOS 10.13 or later is needed".into())
            })
            .clone()
    }

    /// Decode the primary image of a HEIC.
    ///
    /// # Errors
    /// [`Error::Unavailable`] when [`available`] says no,
    /// [`Error::TooLarge`] above `max_pixels`, [`Error::Decode`] for
    /// anything `ImageIO` or Quartz rejects.
    pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Raw, Error> {
        available().map_err(Error::Unavailable)?;
        let data = CFData::from_bytes(bytes);
        // SAFETY: `data` is a live CFData; no options.
        let source = unsafe { CGImageSource::with_data(&data, None) }
            .ok_or_else(|| Error::Decode("ImageIO could not open the file".into()))?;
        // SAFETY: `source` is live; index 0 is the primary image.
        let image = unsafe { source.image_at_index(0, None) }
            .ok_or_else(|| Error::Decode("ImageIO could not decode the primary image".into()))?;
        let image = &*image;

        let width = CGImage::width(Some(image));
        let height = CGImage::height(Some(image));
        let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err(Error::Decode("ImageIO reported an oversized image".into()));
        };
        if w == 0 || h == 0 {
            return Err(Error::Decode("ImageIO reported an empty image".into()));
        }
        let pixels = u64::from(w) * u64::from(h);
        if pixels > max_pixels {
            return Err(Error::TooLarge {
                pixels,
                limit: max_pixels,
            });
        }

        let alpha = matches!(
            CGImage::alpha_info(Some(image)),
            CGImageAlphaInfo::PremultipliedLast
                | CGImageAlphaInfo::PremultipliedFirst
                | CGImageAlphaInfo::Last
                | CGImageAlphaInfo::First
        );
        let deep = CGImage::bits_per_component(Some(image)) > 8;
        let space = CGImage::color_space(Some(image));
        let model = CGColorSpace::model(space.as_deref());
        let gray = model == CGColorSpaceModel::Monochrome && !alpha;

        // The image's own colour space when the context can use it, so
        // nothing is converted; a device space only when Quartz would
        // have no matching context layout otherwise.
        let context_space = match (gray, model == CGColorSpaceModel::RGB) {
            (true, _) | (false, true) => space,
            (false, false) => CGColorSpace::new_device_rgb(),
        }
        .ok_or_else(|| Error::Decode("Quartz has no colour space for the image".into()))?;
        let alpha_info = if gray {
            CGImageAlphaInfo::None
        } else if alpha {
            CGImageAlphaInfo::PremultipliedLast
        } else {
            CGImageAlphaInfo::NoneSkipLast
        };
        let byte_order = if deep {
            CGImageByteOrderInfo::Order16Little
        } else {
            CGImageByteOrderInfo::OrderDefault
        };
        let bitmap_info = alpha_info.0 | byte_order.0;
        let context_channels = if gray { 1 } else { 4 };
        let bits_per_component = if deep { 16 } else { 8 };
        let bytes_per_row = width * context_channels * bits_per_component / 8;
        // Draw into a buffer this crate owns, then read it back.
        let context = Context {
            space: &context_space,
            bitmap_info,
            width,
            height,
            bits_per_component,
            bytes_per_row,
            rect: CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(f64::from(w), f64::from(h)),
            ),
        };
        let buffer = if deep {
            Samples::U16(context.draw(image, vec![0u16; bytes_per_row * height / 2])?)
        } else {
            Samples::U8(context.draw(image, vec![0u8; bytes_per_row * height])?)
        };

        let (channels, samples) = match (gray, alpha) {
            (true, _) => (1, buffer),
            (false, true) => (4, buffer),
            (false, false) => (3, drop_filler(buffer)),
        };
        Ok(Raw {
            width: w,
            height: h,
            channels,
            samples,
            premultiplied: alpha,
        })
    }

    /// A bitmap context of a fixed layout to draw the decoded image into.
    struct Context<'a> {
        space: &'a CGColorSpace,
        bitmap_info: u32,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        rect: CGRect,
    }

    impl Context<'_> {
        /// Draw `image` into `buffer`, which must be `bytes_per_row *
        /// height` bytes, and hand the buffer back.
        fn draw<T>(&self, image: &CGImage, mut buffer: Vec<T>) -> Result<Vec<T>, Error> {
            debug_assert_eq!(
                buffer.len() * size_of::<T>(),
                self.bytes_per_row * self.height
            );
            // SAFETY: the buffer is `bytes_per_row * height` bytes, `T`
            // matches the component width, and the context is dropped
            // before the buffer is read or moved.
            let context = unsafe {
                CGBitmapContextCreate(
                    buffer.as_mut_ptr().cast(),
                    self.width,
                    self.height,
                    self.bits_per_component,
                    self.bytes_per_row,
                    Some(self.space),
                    self.bitmap_info,
                )
            }
            .ok_or_else(|| {
                Error::Decode(format!(
                    "Quartz refused a {}-bit bitmap context",
                    self.bits_per_component
                ))
            })?;
            CGContext::draw_image(Some(&context), self.rect, Some(image));
            drop(context);
            Ok(buffer)
        }
    }

    /// RGBX to RGB: the skipped fourth byte of an opaque context.
    fn drop_filler(samples: Samples) -> Samples {
        fn strip<T: Copy>(v: &[T]) -> Vec<T> {
            v.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|px| [px[0], px[1], px[2]])
                .collect()
        }
        match samples {
            Samples::U8(v) => Samples::U8(strip(&v)),
            Samples::U16(v) => Samples::U16(strip(&v)),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn filler_is_dropped() {
            assert_eq!(
                drop_filler(Samples::U8(vec![1, 2, 3, 0, 4, 5, 6, 0])),
                Samples::U8(vec![1, 2, 3, 4, 5, 6])
            );
            assert_eq!(
                drop_filler(Samples::U16(vec![1, 2, 3, 0])),
                Samples::U16(vec![1, 2, 3])
            );
        }

        #[test]
        fn this_mac_reads_heic() {
            // Every macOS a Rust toolchain targets in 2026 is past 10.13.
            assert_eq!(available(), Ok(()));
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{Error, Raw, Samples, available, decode};
