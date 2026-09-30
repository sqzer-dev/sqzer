//! The values between the stages, ADR-0010 D1: [`Prepared`] after the
//! colour, range and metadata stages, [`Ready`] after the resize. Each
//! stage takes the value of the one before, so the order of ADR-0007 D2
//! is the only one that compiles.

use std::borrow::Cow;

use sqzer_core::codec::FormatInfo;
use sqzer_core::image::Image;
use sqzer_core::params::Resize;
use sqzer_core::resize::Canvas;
use sqzer_core::{Error, Result};

use crate::resize;

/// An image through [`Sqzer::prepare`](crate::Sqzer::prepare): in sRGB
/// unless the ICC profile was kept, integer samples, metadata as the
/// policy says. Resize it with [`Prepared::resize`], as many times as
/// there are sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub(crate) image: Image,
    pub(crate) info: FormatInfo,
    /// [`Sqzer::max_pixels`](crate::Sqzer::max_pixels) at prepare time,
    /// which also bounds what a resize may make.
    pub(crate) max_pixels: u64,
}

impl Prepared {
    /// The prepared image.
    #[must_use]
    pub fn image(&self) -> &Image {
        &self.image
    }

    /// What the input was detected as.
    #[must_use]
    pub fn info(&self) -> FormatInfo {
        self.info
    }

    /// The resize stage of ADR-0009, once per size: the image cropped and
    /// resampled as `resize` says, into a new image. Each size starts
    /// from this image, never from another size.
    ///
    /// Lanczos3 unless the request names another filter, in linear light
    /// with premultiplied alpha. A cover fit crops in the same pass. A
    /// contain fit is not padded here: [`Ready::canvas`] records the box,
    /// and [`Sqzer::encode`](crate::Sqzer::encode) pads once it knows
    /// whether the encoder takes alpha, which decides the default
    /// background. Orientation was applied by the decoder, so the box is
    /// that of the picture as displayed. An image the request leaves alone
    /// comes back as a copy.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] for a request [`Resize::check`] refuses or
    /// an output over the pixel limit, [`Error::Transform`] for a
    /// resampler refusal.
    pub fn resize(&self, resize: &Resize) -> Result<Ready> {
        resized(
            Cow::Borrowed(&self.image),
            self.info,
            resize,
            self.max_pixels,
        )
    }

    /// [`Prepared::resize`] without the copy when the image is left alone.
    pub(crate) fn into_ready(self, resize: &Resize) -> Result<Ready> {
        resized(Cow::Owned(self.image), self.info, resize, self.max_pixels)
    }
}

/// An image ready for [`Sqzer::encode`](crate::Sqzer::encode): prepared,
/// resized, and still owing the padding of a contain fit, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Ready {
    pub(crate) image: Image,
    pub(crate) info: FormatInfo,
    pub(crate) canvas: Option<Canvas>,
}

impl Ready {
    /// The resized image, before any padding.
    #[must_use]
    pub fn image(&self) -> &Image {
        &self.image
    }

    /// What the input was detected as.
    #[must_use]
    pub fn info(&self) -> FormatInfo {
        self.info
    }

    /// The padding a contain fit still owes, applied by the encode stage.
    #[must_use]
    pub fn canvas(&self) -> Option<Canvas> {
        self.canvas
    }

    /// The size the encoder gets: the canvas when there is one.
    #[must_use]
    pub fn output_size(&self) -> (u32, u32) {
        self.canvas
            .map_or((self.image.width(), self.image.height()), |c| {
                (c.width, c.height)
            })
    }

    /// The padding names a background with alpha below 255, which only an
    /// encoder with alpha can carry (ADR-0010 D3).
    #[must_use]
    pub fn translucent_padding(&self) -> bool {
        self.canvas
            .and_then(|c| c.background)
            .is_some_and(|b| b[3] < u8::MAX)
    }
}

fn resized(
    image: Cow<'_, Image>,
    info: FormatInfo,
    resize: &Resize,
    max_pixels: u64,
) -> Result<Ready> {
    resize.check()?;
    let (width, height) = (image.width(), image.height());
    let Some(geometry) = resize.fit(width, height) else {
        return Ok(Ready {
            image: image.into_owned(),
            info,
            canvas: None,
        });
    };
    let (out_w, out_h) = geometry.output();
    let pixels = u64::from(out_w) * u64::from(out_h);
    if pixels > max_pixels {
        return Err(Error::InvalidParams(format!(
            "the resize makes a {width}x{height} image {out_w}x{out_h}, {pixels} pixels, over \
             the limit of {max_pixels}"
        )));
    }
    let image = if geometry.resamples(width, height) {
        resize::resample(&image, &geometry, resize.filter)?
    } else {
        image.into_owned()
    };
    Ok(Ready {
        image,
        info,
        canvas: geometry.canvas,
    })
}
