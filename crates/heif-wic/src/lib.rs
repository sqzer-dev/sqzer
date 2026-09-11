//! HEIC decoding through the Windows Imaging Component, for `sqzer`'s
//! `native-heif` feature (ADR-0005 D3). WIC's HEIF codec comes from two
//! Microsoft Store packages, the HEIF Image Extension (the container
//! parser) and the HEVC Video Extensions (the decoder), and neither is
//! guaranteed to be installed. `available` finds out by decoding a
//! 769-byte HEIC embedded in this crate through the same path a user's
//! file takes, once per process; enumerating components would only prove
//! a package is present, not that the pair of them works.
//!
//! The container side (brands, size, rotation, ICC) is read in safe Rust
//! by `sqzer-codecs`. This crate asks WIC for straight-alpha RGBA, or gray
//! for a monochrome frame, and hands the samples back untransformed: WIC
//! does not apply `irot`, and `sqzer-codecs` applies the orientation it
//! read from the container.
//!
//! Empty on every OS but Windows.

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use windows::Win32::Foundation::{
        WINCODEC_ERR_COMPONENTINITIALIZEFAILURE, WINCODEC_ERR_COMPONENTNOTFOUND,
    };
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat8bppGray, GUID_WICPixelFormat16bppGray,
        GUID_WICPixelFormat16bppGrayHalf, GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat24bppRGB,
        GUID_WICPixelFormat32bppBGR, GUID_WICPixelFormat32bppBGR101010,
        GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppGrayFloat,
        GUID_WICPixelFormat32bppPBGRA, GUID_WICPixelFormat32bppPRGBA,
        GUID_WICPixelFormat32bppR10G10B10A2, GUID_WICPixelFormat32bppR10G10B10A2HDR10,
        GUID_WICPixelFormat32bppRGB, GUID_WICPixelFormat32bppRGBA,
        GUID_WICPixelFormat32bppRGBA1010102, GUID_WICPixelFormat32bppRGBA1010102XR,
        GUID_WICPixelFormat48bppBGR, GUID_WICPixelFormat48bppRGB, GUID_WICPixelFormat48bppRGBHalf,
        GUID_WICPixelFormat64bppBGRA, GUID_WICPixelFormat64bppPBGRA, GUID_WICPixelFormat64bppPRGBA,
        GUID_WICPixelFormat64bppRGB, GUID_WICPixelFormat64bppRGBA,
        GUID_WICPixelFormat64bppRGBAHalf, GUID_WICPixelFormat128bppPRGBAFloat,
        GUID_WICPixelFormat128bppRGBAFloat, GUID_WICPixelFormat128bppRGBFloat, IWICImagingFactory,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::core::GUID;

    /// `tests/fixtures/pattern-rgb.heic`: 48 x 32, HEVC Main 4:2:0, above
    /// the 8 x 8 minimum the HEVC extension accepts.
    const PROBE: &[u8] = include_bytes!("../probe.heic");

    /// Interleaved, tightly packed samples from one decode.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Samples {
        /// 8 bits per sample.
        U8(Vec<u8>),
        /// 16 bits per sample, full range.
        U16(Vec<u16>),
    }

    /// One decoded frame: `width * height * channels` samples, row-major,
    /// straight alpha, in the container's stored orientation.
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
    }

    /// Why a decode did not happen.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Error {
        /// WIC cannot decode HEIC on this machine. The string is the
        /// reason [`available`] gives.
        Unavailable(String),
        /// The image is larger than the caller allows.
        TooLarge {
            /// Pixels in the image.
            pixels: u64,
            /// The caller's limit.
            limit: u64,
        },
        /// WIC refused the file. The string names the step and the
        /// `HRESULT`.
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

    /// Whether WIC can decode a `.heic` on this machine, found out by
    /// decoding the embedded probe once per process.
    ///
    /// # Errors
    /// What is missing, phrased for the user, with the `HRESULT` WIC gave.
    pub fn available() -> Result<(), String> {
        static PROBED: OnceLock<Result<(), String>> = OnceLock::new();
        PROBED
            .get_or_init(|| match decode_frame(PROBE, u64::MAX) {
                Ok(raw) if raw.width == 48 && raw.height == 32 => Ok(()),
                Ok(raw) => Err(format!(
                    "WIC decoded the 48x32 probe image as {}x{}",
                    raw.width, raw.height
                )),
                Err(e) => Err(e.to_string()),
            })
            .clone()
    }

    /// Decode the primary image of a HEIC.
    ///
    /// # Errors
    /// [`Error::Unavailable`] when [`available`] says no,
    /// [`Error::TooLarge`] above `max_pixels`, [`Error::Decode`] for
    /// anything WIC rejects.
    pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Raw, Error> {
        available().map_err(Error::Unavailable)?;
        decode_frame(bytes, max_pixels)
    }

    /// Balances `CoInitializeEx` with `CoUninitialize` on the thread, and
    /// leaves an apartment somebody else initialised alone.
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            // SAFETY: no preconditions; a thread that is already
            // initialised in another mode gets `RPC_E_CHANGED_MODE`,
            // which is a failure code and so is not balanced below.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Self(hr.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: matches the successful `CoInitializeEx` above,
                // on the same thread.
                unsafe { CoUninitialize() }
            }
        }
    }

    fn step(what: &str) -> impl FnOnce(windows::core::Error) -> Error + '_ {
        move |e| Error::Decode(format!("WIC {what}: {e}"))
    }

    fn decode_frame(bytes: &[u8], max_pixels: u64) -> Result<Raw, Error> {
        let _com = Com::init();
        // SAFETY: COM calls with valid arguments; every interface pointer
        // is owned by a `windows` smart pointer and released on drop.
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(step("imaging factory"))?;
            let stream = factory.CreateStream().map_err(step("stream"))?;
            // `bytes` outlives the stream: both live to the end of this
            // function.
            stream.InitializeFromMemory(bytes).map_err(step("stream"))?;
            let decoder = factory
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .map_err(|e| {
                    // Seen on GitHub's windows-latest (Windows Server, no
                    // Store packages): the HEIF container decoder is
                    // registered but fails to initialise, 0x88982F8B, so
                    // the "not found" case is a machine with no HEIF
                    // codec at all.
                    if e.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
                        Error::Decode(format!(
                            "HEIF Image Extension is not installed (Microsoft Store: \
                             \"HEIF Image Extensions\"); {e}"
                        ))
                    } else if e.code() == WINCODEC_ERR_COMPONENTINITIALIZEFAILURE {
                        Error::Decode(format!(
                            "the HEIF codec could not initialise, which is what a missing \
                             HEVC Video Extensions package looks like (Microsoft Store: \
                             \"HEVC Video Extensions\"); {e}"
                        ))
                    } else {
                        Error::Decode(format!("WIC cannot open HEIC: {e}"))
                    }
                })?;
            let frame = decoder.GetFrame(0).map_err(|e| {
                Error::Decode(format!(
                    "HEVC Video Extensions are not installed, or the HEIF codec failed \
                     (Microsoft Store: \"HEVC Video Extensions\"); {e}"
                ))
            })?;
            let (mut width, mut height) = (0u32, 0u32);
            frame
                .GetSize(&raw mut width, &raw mut height)
                .map_err(step("frame size"))?;
            if width == 0 || height == 0 {
                return Err(Error::Decode("WIC reported an empty frame".into()));
            }
            let pixels = u64::from(width) * u64::from(height);
            if pixels > max_pixels {
                return Err(Error::TooLarge {
                    pixels,
                    limit: max_pixels,
                });
            }
            let source = frame.GetPixelFormat().map_err(step("pixel format"))?;
            let layout = Layout::of(&source);
            let target = match (layout.gray, layout.deep) {
                (true, false) => GUID_WICPixelFormat8bppGray,
                (true, true) => GUID_WICPixelFormat16bppGray,
                (false, false) => GUID_WICPixelFormat32bppRGBA,
                (false, true) => GUID_WICPixelFormat64bppRGBA,
            };
            let converter = factory
                .CreateFormatConverter()
                .map_err(step("format converter"))?;
            converter
                .Initialize(
                    &frame,
                    &raw const target,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .map_err(step("format conversion"))?;
            let channels: u32 = if layout.gray { 1 } else { 4 };
            let bytes_per_sample: u32 = if layout.deep { 2 } else { 1 };
            let stride = width * channels * bytes_per_sample;
            let mut buffer = vec![0u8; stride as usize * height as usize];
            converter
                .CopyPixels(std::ptr::null(), stride, &mut buffer)
                .map_err(|e| {
                    Error::Decode(format!(
                        "HEVC Video Extensions are not installed, or the HEVC decoder failed \
                         (Microsoft Store: \"HEVC Video Extensions\"); {e}"
                    ))
                })?;
            Ok(pack(width, height, layout, buffer))
        }
    }

    /// What the frame's pixel format says about the picture.
    #[derive(Clone, Copy)]
    struct Layout {
        gray: bool,
        deep: bool,
        alpha: bool,
    }

    impl Layout {
        /// Classify a WIC pixel format. Unknown formats are taken as
        /// 8-bit RGBA, which every converter can produce.
        fn of(format: &GUID) -> Self {
            let gray = [
                GUID_WICPixelFormat8bppGray,
                GUID_WICPixelFormat16bppGray,
                GUID_WICPixelFormat16bppGrayHalf,
                GUID_WICPixelFormat32bppGrayFloat,
            ]
            .contains(format);
            let eight_bit = [
                GUID_WICPixelFormat8bppGray,
                GUID_WICPixelFormat24bppBGR,
                GUID_WICPixelFormat24bppRGB,
                GUID_WICPixelFormat32bppBGR,
                GUID_WICPixelFormat32bppRGB,
                GUID_WICPixelFormat32bppBGRA,
                GUID_WICPixelFormat32bppPBGRA,
                GUID_WICPixelFormat32bppRGBA,
                GUID_WICPixelFormat32bppPRGBA,
            ]
            .contains(format);
            let deep = [
                GUID_WICPixelFormat16bppGray,
                GUID_WICPixelFormat16bppGrayHalf,
                GUID_WICPixelFormat32bppGrayFloat,
                GUID_WICPixelFormat48bppRGB,
                GUID_WICPixelFormat48bppBGR,
                GUID_WICPixelFormat48bppRGBHalf,
                GUID_WICPixelFormat64bppRGB,
                GUID_WICPixelFormat64bppRGBA,
                GUID_WICPixelFormat64bppBGRA,
                GUID_WICPixelFormat64bppPRGBA,
                GUID_WICPixelFormat64bppPBGRA,
                GUID_WICPixelFormat64bppRGBAHalf,
                GUID_WICPixelFormat32bppRGBA1010102,
                GUID_WICPixelFormat32bppRGBA1010102XR,
                GUID_WICPixelFormat32bppR10G10B10A2,
                GUID_WICPixelFormat32bppR10G10B10A2HDR10,
                GUID_WICPixelFormat32bppBGR101010,
                GUID_WICPixelFormat128bppRGBFloat,
                GUID_WICPixelFormat128bppRGBAFloat,
                GUID_WICPixelFormat128bppPRGBAFloat,
            ]
            .contains(format);
            let opaque = [
                GUID_WICPixelFormat8bppGray,
                GUID_WICPixelFormat16bppGray,
                GUID_WICPixelFormat16bppGrayHalf,
                GUID_WICPixelFormat32bppGrayFloat,
                GUID_WICPixelFormat24bppBGR,
                GUID_WICPixelFormat24bppRGB,
                GUID_WICPixelFormat32bppBGR,
                GUID_WICPixelFormat32bppRGB,
                GUID_WICPixelFormat48bppRGB,
                GUID_WICPixelFormat48bppBGR,
                GUID_WICPixelFormat48bppRGBHalf,
                GUID_WICPixelFormat64bppRGB,
                GUID_WICPixelFormat32bppBGR101010,
                GUID_WICPixelFormat128bppRGBFloat,
            ]
            .contains(format);
            debug_assert!(!(eight_bit && deep));
            Self {
                gray,
                deep,
                alpha: !opaque,
            }
        }
    }

    /// Turn the converter's buffer into `Raw`, dropping the alpha channel
    /// WIC filled in for an opaque source.
    fn pack(width: u32, height: u32, layout: Layout, buffer: Vec<u8>) -> Raw {
        let channels: u8 = match (layout.gray, layout.alpha) {
            (true, _) => 1,
            (false, true) => 4,
            (false, false) => 3,
        };
        let samples = if layout.deep {
            let wide: Vec<u16> = buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect();
            Samples::U16(if channels == 3 {
                drop_alpha(&wide)
            } else {
                wide
            })
        } else {
            Samples::U8(if channels == 3 {
                drop_alpha(&buffer)
            } else {
                buffer
            })
        };
        Raw {
            width,
            height,
            channels,
            samples,
        }
    }

    fn drop_alpha<T: Copy>(rgba: &[T]) -> Vec<T> {
        rgba.as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn layouts_are_classified() {
            let l = Layout::of(&GUID_WICPixelFormat32bppBGRA);
            assert!(!l.gray && !l.deep && l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat24bppBGR);
            assert!(!l.gray && !l.deep && !l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat8bppGray);
            assert!(l.gray && !l.deep && !l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat64bppRGBA);
            assert!(!l.gray && l.deep && l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat16bppGray);
            assert!(l.gray && l.deep && !l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat48bppRGB);
            assert!(!l.gray && l.deep && !l.alpha);
            let l = Layout::of(&GUID_WICPixelFormat32bppRGBA1010102);
            assert!(!l.gray && l.deep && l.alpha);
            // Unknown: 8-bit RGBA, the conversion that always works.
            let l = Layout::of(&GUID::zeroed());
            assert!(!l.gray && !l.deep && l.alpha);
        }

        #[test]
        fn opaque_frames_lose_the_filler_alpha() {
            let raw = pack(
                2,
                1,
                Layout {
                    gray: false,
                    deep: false,
                    alpha: false,
                },
                vec![1, 2, 3, 255, 4, 5, 6, 255],
            );
            assert_eq!(raw.channels, 3);
            assert_eq!(raw.samples, Samples::U8(vec![1, 2, 3, 4, 5, 6]));
            let raw = pack(
                1,
                1,
                Layout {
                    gray: false,
                    deep: true,
                    alpha: true,
                },
                vec![1, 0, 2, 0, 3, 0, 0xff, 0xff],
            );
            assert_eq!(raw.channels, 4);
            assert_eq!(raw.samples, Samples::U16(vec![1, 2, 3, 65535]));
        }

        #[test]
        fn the_probe_is_the_rgb_fixture() {
            assert_eq!(PROBE.len(), 769);
            assert_eq!(&PROBE[4..8], b"ftyp");
        }
    }
}

#[cfg(windows)]
pub use imp::{Error, Raw, Samples, available, decode};
