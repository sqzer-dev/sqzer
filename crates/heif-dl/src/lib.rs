//! `libheif` loaded at runtime, so a `sqzer` binary built with
//! `native-heif` starts on a machine that has no `libheif` and reads HEIC
//! on one that does (ADR-0005 D8). Nothing links `libheif`: the library is
//! opened with `dlopen` (or `LoadLibrary`) the first time HEIC is in play,
//! its version is checked against the 1.17 floor, and the handful of
//! functions the decoder needs are read out as function pointers.
//!
//! Two entry points: [`available`], which says whether a usable `libheif`
//! with an HEVC decoder is on this machine, and [`decode`], which hands
//! back interleaved samples. The container side (brands, size, rotation,
//! ICC) is read in safe Rust by `sqzer-codecs`; this crate only decodes.
//!
//! The library applies the container's own crop, rotation and mirroring
//! (`clap`, `irot`, `imir`) because it cannot skip one without the others,
//! so a [`Raw`] from here is already upright. This is the one place where
//! a HEIC backend's own transform handling is used; see `native::heif`
//! in `sqzer-codecs` for how that is reconciled with the other two.
//!
//! Where the library is looked for, first match wins:
//!
//! ```text
//! any      the file named by SQZER_LIBHEIF, or the default name inside
//!          the directory it names
//! Linux    libheif.so.1 on the loader's default path
//! macOS    libheif.1.dylib on the loader's default path, then under
//!          /opt/homebrew/lib and /usr/local/lib, which dyld does not
//!          search on its own
//! Windows  heif.dll (vcpkg's name) then libheif.dll (msys2's) on PATH
//! ```
//!
//! A statically linked musl binary cannot `dlopen` at all; `sqzer-codecs`
//! compiles this crate out for `target_env = "musl"`.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

use libloading::Library;

/// Environment variable naming the `libheif` to load: a library file, or a
/// directory holding one under the platform's default name.
pub const LIBRARY_ENV: &str = "SQZER_LIBHEIF";

/// Oldest `libheif` accepted, as `heif_get_version_number` encodes it:
/// 1.17.0, the floor ADR-0004 set.
const MIN_VERSION: u32 = 0x01_11_00_00;

/// The platform's default file name for the library.
pub const LIBRARY_NAME: &str = if cfg!(target_os = "macos") {
    "libheif.1.dylib"
} else if cfg!(windows) {
    "heif.dll"
} else {
    "libheif.so.1"
};

/// Interleaved, tightly packed samples from one decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Samples {
    /// 8 bits per sample.
    U8(Vec<u8>),
    /// 16 bits per sample, scaled to the full range from the source's 10
    /// or 12 bits.
    U16(Vec<u16>),
}

/// One decoded image: `width * height * channels` samples, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    /// Width in pixels, after the container's transforms.
    pub width: u32,
    /// Height in pixels, after the container's transforms.
    pub height: u32,
    /// 1 gray, 2 gray plus alpha, 3 RGB, 4 RGBA.
    pub channels: u8,
    /// The samples.
    pub samples: Samples,
    /// Whether the colour samples are premultiplied by alpha, as the
    /// file's `prem` flag says.
    pub premultiplied: bool,
}

/// Why a decode did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No usable `libheif` on this machine. The string is the reason
    /// [`available`] gives.
    Unavailable(String),
    /// The image is larger than the caller allows.
    TooLarge {
        /// Pixels in the image.
        pixels: u64,
        /// The caller's limit.
        limit: u64,
    },
    /// `libheif` refused the file. The string is its own message.
    Decode(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable(reason) => f.write_str(reason),
            Self::TooLarge { pixels, limit } => {
                write!(f, "image has {pixels} pixels, limit is {limit}")
            }
            Self::Decode(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

/// Whether a usable `libheif` is on this machine: found, at least 1.17,
/// initialised, and with an HEVC decoder. Probed once per process.
///
/// # Errors
/// What is missing, phrased for the user.
pub fn available() -> Result<(), String> {
    library().map(|_| ()).map_err(Clone::clone)
}

/// The loaded library's version as `(major, minor, patch)`, if it loaded.
#[must_use]
pub fn version() -> Option<(u8, u8, u8)> {
    library().ok().map(|lib| lib.version)
}

/// Decode the primary image of a HEIC.
///
/// # Errors
/// [`Error::Unavailable`] when [`available`] says no, [`Error::TooLarge`]
/// above `max_pixels`, [`Error::Decode`] for anything the library rejects.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Raw, Error> {
    let lib = library().map_err(|e| Error::Unavailable(e.clone()))?;
    // SAFETY: `lib` was loaded and initialised by `library`; `bytes` stays
    // alive for the whole call, which the "without copy" reader needs.
    unsafe { decode_with(lib, bytes, max_pixels) }
}

// ---------------------------------------------------------------- FFI

/// `struct heif_error`, returned by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

impl HeifError {
    /// `Ok(())` for `heif_error_Ok`, else the library's message.
    unsafe fn check(self) -> Result<(), Error> {
        if self.code == 0 {
            return Ok(());
        }
        let message = if self.message.is_null() {
            String::from("unknown error")
        } else {
            // SAFETY: libheif guarantees `message` is a NUL-terminated
            // string with static lifetime.
            unsafe { CStr::from_ptr(self.message) }
                .to_string_lossy()
                .into_owned()
        };
        Err(Error::Decode(message))
    }
}

/// The version 2 prefix of `struct heif_decoding_options`. The library
/// allocates the real struct and stamps its version in the first byte;
/// only fields within a version the library has are written.
#[repr(C)]
struct DecodingOptionsV2 {
    version: u8,
    ignore_transformations: u8,
    start_progress: *const c_void,
    on_progress: *const c_void,
    end_progress: *const c_void,
    progress_user_data: *mut c_void,
    convert_hdr_to_8bit: u8,
}

const HEIF_COMPRESSION_HEVC: c_int = 1;
const HEIF_COLORSPACE_RGB: c_int = 1;
const HEIF_COLORSPACE_MONOCHROME: c_int = 2;
const HEIF_CHROMA_MONOCHROME: c_int = 0;
const HEIF_CHROMA_INTERLEAVED_RGB: c_int = 10;
const HEIF_CHROMA_INTERLEAVED_RGBA: c_int = 11;
const HEIF_CHROMA_INTERLEAVED_RRGGBB_LE: c_int = 14;
const HEIF_CHROMA_INTERLEAVED_RRGGBBAA_LE: c_int = 15;
const HEIF_CHANNEL_Y: c_int = 0;
const HEIF_CHANNEL_ALPHA: c_int = 6;
const HEIF_CHANNEL_INTERLEAVED: c_int = 10;

/// Declares the function-pointer table and the loader that fills it, one
/// `dlsym` per function. Every signature is a C-ABI function over opaque
/// pointers and `int`-sized enums, copied from `libheif/heif.h`.
macro_rules! api {
    ($( $name:ident : fn($($arg:ty),*) $(-> $ret:ty)?; )*) => {
        struct Api {
            _lib: Library,
            $( $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        impl Api {
            /// # Safety
            /// The library must be a `libheif` whose exported functions
            /// have the declared signatures.
            unsafe fn load(lib: Library) -> Result<Self, String> {
                $(
                    // SAFETY: the symbol is looked up by name and its
                    // type is the C prototype of that function.
                    let $name = unsafe {
                        lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                            concat!(stringify!($name), "\0").as_bytes(),
                        )
                    }
                    .map(|symbol| *symbol)
                    .map_err(|e| format!("`{}` is missing: {e}", stringify!($name)))?;
                )*
                Ok(Self { _lib: lib, $( $name, )* })
            }
        }
    };
}

api! {
    heif_init: fn(*mut c_void) -> HeifError;
    heif_get_version_number: fn() -> u32;
    heif_get_decoder_descriptors: fn(c_int, *mut *const c_void, c_int) -> c_int;
    heif_context_alloc: fn() -> *mut c_void;
    heif_context_free: fn(*mut c_void);
    heif_context_read_from_memory_without_copy: fn(*mut c_void, *const c_void, usize, *const c_void) -> HeifError;
    heif_context_set_max_decoding_threads: fn(*mut c_void, c_int);
    heif_context_get_primary_image_handle: fn(*mut c_void, *mut *mut c_void) -> HeifError;
    heif_image_handle_release: fn(*const c_void);
    heif_image_handle_get_width: fn(*const c_void) -> c_int;
    heif_image_handle_get_height: fn(*const c_void) -> c_int;
    heif_image_handle_get_luma_bits_per_pixel: fn(*const c_void) -> c_int;
    heif_image_handle_has_alpha_channel: fn(*const c_void) -> c_int;
    heif_image_handle_is_premultiplied_alpha: fn(*const c_void) -> c_int;
    heif_image_handle_get_preferred_decoding_colorspace: fn(*const c_void, *mut c_int, *mut c_int) -> HeifError;
    heif_decoding_options_alloc: fn() -> *mut c_void;
    heif_decoding_options_free: fn(*mut c_void);
    heif_decode_image: fn(*const c_void, *mut *mut c_void, c_int, c_int, *const c_void) -> HeifError;
    heif_image_release: fn(*const c_void);
    heif_image_has_channel: fn(*const c_void, c_int) -> c_int;
    heif_image_get_width: fn(*const c_void, c_int) -> c_int;
    heif_image_get_height: fn(*const c_void, c_int) -> c_int;
    heif_image_get_bits_per_pixel_range: fn(*const c_void, c_int) -> c_int;
    heif_image_get_plane_readonly: fn(*const c_void, c_int, *mut c_int) -> *const u8;
}

/// A loaded, checked, initialised library.
struct Loaded {
    api: Api,
    version: (u8, u8, u8),
}

// SAFETY: the table holds plain function pointers and the `Library`
// handle, which libloading marks Send + Sync; libheif's own state is
// guarded internally and every context here is per call.
unsafe impl Sync for Loaded {}
// SAFETY: as above.
unsafe impl Send for Loaded {}

fn library() -> Result<&'static Loaded, &'static String> {
    static LIB: OnceLock<Result<Loaded, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        // SAFETY: loading libheif runs its constructors, which only set up
        // its own globals; nothing here relies on any other library's
        // state at load time.
        unsafe { open() }
    })
    .as_ref()
}

/// Where to look, in order.
fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(given) = std::env::var_os(LIBRARY_ENV) {
        let given = PathBuf::from(given);
        if given.is_dir() {
            paths.push(given.join(LIBRARY_NAME));
        } else {
            paths.push(given);
        }
    }
    paths.push(PathBuf::from(LIBRARY_NAME));
    if cfg!(target_os = "macos") {
        paths.push(PathBuf::from("/opt/homebrew/lib").join(LIBRARY_NAME));
        paths.push(PathBuf::from("/usr/local/lib").join(LIBRARY_NAME));
    }
    if cfg!(windows) {
        paths.push(PathBuf::from("libheif.dll"));
    }
    paths
}

/// How to get the library, per platform, for the not-found message.
const INSTALL_HINT: &str = if cfg!(target_os = "macos") {
    "brew install libheif"
} else if cfg!(windows) {
    "put a libheif build's heif.dll on PATH"
} else {
    "Debian/Ubuntu: apt install libheif1 libheif-plugin-libde265; Fedora: libheif-freeworld from RPM Fusion"
};

unsafe fn open() -> Result<Loaded, String> {
    for path in candidates() {
        // SAFETY: see `library`.
        let Ok(lib) = (unsafe { Library::new(&path) }) else {
            continue;
        };
        // SAFETY: the file loaded under libheif's name; `load` checks
        // every symbol exists before the table is used.
        let api = unsafe { Api::load(lib) }
            .map_err(|e| format!("{} is not a usable libheif: {e}", path.display()))?;
        // SAFETY: `heif_get_version_number` takes nothing and reads a
        // constant.
        let bcd = unsafe { (api.heif_get_version_number)() };
        let version = (
            (bcd >> 24) as u8,
            ((bcd >> 16) & 0xff) as u8,
            ((bcd >> 8) & 0xff) as u8,
        );
        let name = format!("libheif {}.{}.{}", version.0, version.1, version.2);
        if bcd < MIN_VERSION {
            return Err(format!(
                "{name} at {} is older than 1.17, the oldest sqzer supports",
                path.display()
            ));
        }
        // SAFETY: a null `heif_init_params` asks for the defaults.
        let init = unsafe { (api.heif_init)(std::ptr::null_mut()).check() };
        // `heif_init` reports the last plugin it failed to load, which on
        // a distribution with a half-installed plugin directory says
        // nothing about the decoders it did load. The decoder list is
        // the answer; the init error is kept for the message when the
        // list is empty.
        // SAFETY: a null output array with a count of zero asks only how
        // many decoders there are.
        let hevc = unsafe {
            (api.heif_get_decoder_descriptors)(HEIF_COMPRESSION_HEVC, std::ptr::null_mut(), 0)
        };
        if hevc <= 0 {
            let plugins = match init {
                Ok(()) => String::new(),
                Err(e) => format!("; plugin loading failed: {e}"),
            };
            return Err(format!(
                "{name} at {} has no HEVC decoder plugin ({INSTALL_HINT}){plugins}",
                path.display()
            ));
        }
        return Ok(Loaded { api, version });
    }
    Err(format!(
        "{LIBRARY_NAME} not found ({INSTALL_HINT}, or point {LIBRARY_ENV} at it)"
    ))
}

// ------------------------------------------------------------- decoding

/// Frees a libheif object on every exit path.
struct Guard<'a> {
    ptr: *mut c_void,
    free: unsafe extern "C" fn(*mut c_void),
    _api: &'a Api,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `ptr` came from the matching allocator and is freed
            // exactly once, here.
            unsafe { (self.free)(self.ptr) }
        }
    }
}

/// `heif_image_handle_release` and `heif_image_release` take `const`
/// pointers; this adapts them to the guard's signature.
struct ConstGuard<'a> {
    ptr: *mut c_void,
    free: unsafe extern "C" fn(*const c_void),
    _api: &'a Api,
}

impl Drop for ConstGuard<'_> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: as for `Guard`.
            unsafe { (self.free)(self.ptr) }
        }
    }
}

fn check_pixels(width: c_int, height: c_int, limit: u64) -> Result<(u32, u32), Error> {
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(Error::Decode("libheif reported a negative size".into()));
    };
    if width == 0 || height == 0 {
        return Err(Error::Decode("libheif reported an empty image".into()));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limit {
        return Err(Error::TooLarge { pixels, limit });
    }
    Ok((width, height))
}

unsafe fn decode_with(lib: &Loaded, bytes: &[u8], max_pixels: u64) -> Result<Raw, Error> {
    let api = &lib.api;
    // SAFETY: plain allocation; the guard frees it.
    let ctx = unsafe { (api.heif_context_alloc)() };
    let ctx = Guard {
        ptr: ctx,
        free: api.heif_context_free,
        _api: api,
    };
    if ctx.ptr.is_null() {
        return Err(Error::Decode("libheif could not allocate a context".into()));
    }
    // SAFETY: `bytes` outlives the context (both live to the end of this
    // function); a null reading-options pointer means the defaults.
    unsafe {
        (api.heif_context_read_from_memory_without_copy)(
            ctx.ptr,
            bytes.as_ptr().cast(),
            bytes.len(),
            std::ptr::null(),
        )
        .check()?;
        // Zero decodes on the calling thread; one would spawn a worker.
        (api.heif_context_set_max_decoding_threads)(ctx.ptr, 0);
    }
    let mut handle_ptr = std::ptr::null_mut();
    // SAFETY: `ctx.ptr` is a live context; the handle is released by the
    // guard.
    unsafe { (api.heif_context_get_primary_image_handle)(ctx.ptr, &raw mut handle_ptr).check()? };
    let handle = ConstGuard {
        ptr: handle_ptr,
        free: api.heif_image_handle_release,
        _api: api,
    };
    // SAFETY: the handle is live for every call below.
    let shape = unsafe { inspect(api, handle.ptr.cast_const(), max_pixels)? };
    let Shape {
        wide,
        alpha,
        premultiplied,
        mono,
    } = shape;
    // SAFETY: plain allocation; the guard frees it.
    let options = unsafe { decoding_options(api)? };

    let (colorspace, chroma) = if mono {
        (HEIF_COLORSPACE_MONOCHROME, HEIF_CHROMA_MONOCHROME)
    } else {
        let chroma = match (alpha, wide) {
            (false, false) => HEIF_CHROMA_INTERLEAVED_RGB,
            (true, false) => HEIF_CHROMA_INTERLEAVED_RGBA,
            (false, true) => HEIF_CHROMA_INTERLEAVED_RRGGBB_LE,
            (true, true) => HEIF_CHROMA_INTERLEAVED_RRGGBBAA_LE,
        };
        (HEIF_COLORSPACE_RGB, chroma)
    };
    let mut image_ptr = std::ptr::null_mut();
    // SAFETY: handle and options are live; the image is released by the
    // guard.
    unsafe {
        (api.heif_decode_image)(
            handle.ptr.cast_const(),
            &raw mut image_ptr,
            colorspace,
            chroma,
            options.ptr.cast_const(),
        )
        .check()?;
    }
    let image = ConstGuard {
        ptr: image_ptr,
        free: api.heif_image_release,
        _api: api,
    };
    let img = image.ptr.cast_const();

    if mono {
        // SAFETY: `img` is a live decoded image.
        return unsafe { mono_raw(api, img, alpha, wide, premultiplied, max_pixels) };
    }
    let channels: u8 = if alpha { 4 } else { 3 };
    // SAFETY: `img` is a live decoded image.
    let (width, height, samples) = unsafe {
        plane(
            api,
            img,
            HEIF_CHANNEL_INTERLEAVED,
            channels,
            wide,
            max_pixels,
        )?
    };
    Ok(Raw {
        width,
        height,
        channels,
        samples,
        premultiplied,
    })
}

/// A monochrome decode: the luma plane, with the alpha plane interleaved
/// in when the file has one.
unsafe fn mono_raw(
    api: &Api,
    img: *const c_void,
    alpha: bool,
    wide: bool,
    premultiplied: bool,
    max_pixels: u64,
) -> Result<Raw, Error> {
    // SAFETY: `img` is a live decoded image, per the caller.
    let (width, height, luma) = unsafe { plane(api, img, HEIF_CHANNEL_Y, 1, wide, max_pixels)? };
    // SAFETY: as above.
    let has_alpha = alpha && unsafe { (api.heif_image_has_channel)(img, HEIF_CHANNEL_ALPHA) } != 0;
    if !has_alpha {
        return Ok(Raw {
            width,
            height,
            channels: 1,
            samples: luma,
            premultiplied: false,
        });
    }
    // SAFETY: as above.
    let (aw, ah, alpha) = unsafe { plane(api, img, HEIF_CHANNEL_ALPHA, 1, wide, max_pixels)? };
    if (aw, ah) != (width, height) {
        return Err(Error::Decode(format!(
            "alpha plane is {aw}x{ah}, luma plane is {width}x{height}"
        )));
    }
    let samples = match (luma, alpha) {
        (Samples::U8(l), Samples::U8(a)) => {
            Samples::U8(l.iter().zip(&a).flat_map(|(&l, &a)| [l, a]).collect())
        }
        (Samples::U16(l), Samples::U16(a)) => {
            Samples::U16(l.iter().zip(&a).flat_map(|(&l, &a)| [l, a]).collect())
        }
        _ => unreachable!("both planes are read at the same depth"),
    };
    Ok(Raw {
        width,
        height,
        channels: 2,
        samples,
        premultiplied,
    })
}

/// What the primary image handle says about the picture: four facts, not
/// a state machine.
#[allow(clippy::struct_excessive_bools)]
struct Shape {
    wide: bool,
    alpha: bool,
    premultiplied: bool,
    mono: bool,
}

/// Read the handle's size, depth, alpha and preferred colourspace, and
/// check the size against `max_pixels`.
unsafe fn inspect(api: &Api, handle: *const c_void, max_pixels: u64) -> Result<Shape, Error> {
    // SAFETY: the caller guarantees a live handle.
    unsafe {
        check_pixels(
            (api.heif_image_handle_get_width)(handle),
            (api.heif_image_handle_get_height)(handle),
            max_pixels,
        )?;
        let mut colorspace: c_int = 0;
        let mut chroma: c_int = 0;
        let preferred = (api.heif_image_handle_get_preferred_decoding_colorspace)(
            handle,
            &raw mut colorspace,
            &raw mut chroma,
        );
        Ok(Shape {
            wide: (api.heif_image_handle_get_luma_bits_per_pixel)(handle) > 8,
            alpha: (api.heif_image_handle_has_alpha_channel)(handle) != 0,
            premultiplied: (api.heif_image_handle_is_premultiplied_alpha)(handle) != 0,
            mono: preferred.code == 0 && colorspace == HEIF_COLORSPACE_MONOCHROME,
        })
    }
}

/// Decoding options with HDR-to-8-bit conversion off, so 10 and 12-bit
/// sources keep their depth.
unsafe fn decoding_options(api: &Api) -> Result<Guard<'_>, Error> {
    // SAFETY: plain allocation; the guard frees it.
    let options = unsafe { (api.heif_decoding_options_alloc)() };
    let options = Guard {
        ptr: options,
        free: api.heif_decoding_options_free,
        _api: api,
    };
    if options.ptr.is_null() {
        return Err(Error::Decode(
            "libheif could not allocate decoding options".into(),
        ));
    }
    // SAFETY: the library allocated at least a version-1 struct and wrote
    // its version into the first byte; `convert_hdr_to_8bit` exists from
    // version 2 on, and is only written when the library says so.
    unsafe {
        let version = *options.ptr.cast::<u8>();
        if version >= 2 {
            (*options.ptr.cast::<DecodingOptionsV2>()).convert_hdr_to_8bit = 0;
        }
    }
    Ok(options)
}

/// Copy one plane out of a decoded image: `channels` samples per pixel,
/// 16-bit little-endian samples when `wide`, scaled to the full 16-bit
/// range from the plane's own depth.
unsafe fn plane(
    api: &Api,
    img: *const c_void,
    channel: c_int,
    channels: u8,
    wide: bool,
    max_pixels: u64,
) -> Result<(u32, u32, Samples), Error> {
    // SAFETY: `img` is a live image; the plane pointer stays valid until
    // the image is released, which is after this function returns.
    let (width, height, bits, data) = unsafe {
        let (width, height) = check_pixels(
            (api.heif_image_get_width)(img, channel),
            (api.heif_image_get_height)(img, channel),
            max_pixels,
        )?;
        let bits = (api.heif_image_get_bits_per_pixel_range)(img, channel);
        let mut stride: c_int = 0;
        let ptr = (api.heif_image_get_plane_readonly)(img, channel, &raw mut stride);
        if ptr.is_null() {
            return Err(Error::Decode("libheif returned no plane".into()));
        }
        let bytes_per_pixel = usize::from(channels) * if wide { 2 } else { 1 };
        let row_bytes = width as usize * bytes_per_pixel;
        let stride = usize::try_from(stride).unwrap_or(0);
        if stride < row_bytes {
            return Err(Error::Decode(
                "libheif plane is narrower than its geometry".into(),
            ));
        }
        // The last row need not be padded out to a full stride.
        let total = stride * (height as usize - 1) + row_bytes;
        let data = std::slice::from_raw_parts(ptr, total);
        let rows = data
            .chunks(stride)
            .take(height as usize)
            .map(|row| &row[..row_bytes]);
        (width, height, bits, rows.collect::<Vec<_>>())
    };
    if !wide {
        return Ok((width, height, Samples::U8(data.concat())));
    }
    let mut out: Vec<u16> = data
        .iter()
        .flat_map(|row| row.as_chunks::<2>().0)
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    widen(&mut out, bits);
    Ok((width, height, Samples::U16(out)))
}

/// Scale `bits`-bit samples to 16 bits, replicating the top bits so the
/// endpoints land on 0 and 65535. Anything but 10 or 12 bits is left as
/// it is.
fn widen(v: &mut [u16], bits: c_int) {
    match bits {
        10 => v.iter_mut().for_each(|s| *s = (*s << 6) | (*s >> 4)),
        12 => v.iter_mut().for_each(|s| *s = (*s << 4) | (*s >> 8)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widen_hits_the_endpoints() {
        let mut v = [0u16, 1023];
        widen(&mut v, 10);
        assert_eq!(v, [0, 65535]);
        let mut v = [0u16, 4095];
        widen(&mut v, 12);
        assert_eq!(v, [0, 65535]);
        let mut v = [0u16, 65535];
        widen(&mut v, 16);
        assert_eq!(v, [0, 65535]);
    }

    #[test]
    fn version_floor_is_1_17() {
        assert_eq!(MIN_VERSION, 0x0111_0000);
    }

    #[test]
    fn candidates_honour_the_variable() {
        // The variable is process-wide; this test owns it for its
        // duration and the others do not read it.
        let dir = std::env::temp_dir();
        // SAFETY: single-threaded access within this test; nothing else
        // in this crate's tests reads the variable.
        unsafe { std::env::set_var(LIBRARY_ENV, &dir) };
        let paths = candidates();
        assert_eq!(paths[0], dir.join(LIBRARY_NAME));
        assert_eq!(paths[1], PathBuf::from(LIBRARY_NAME));
        // SAFETY: as above.
        unsafe { std::env::set_var(LIBRARY_ENV, "/nowhere/libheif.so.1") };
        assert_eq!(candidates()[0], PathBuf::from("/nowhere/libheif.so.1"));
        // SAFETY: as above.
        unsafe { std::env::remove_var(LIBRARY_ENV) };
        assert_eq!(candidates()[0], PathBuf::from(LIBRARY_NAME));
    }

    #[test]
    fn errors_read_as_prose() {
        assert_eq!(
            Error::TooLarge {
                pixels: 10,
                limit: 4
            }
            .to_string(),
            "image has 10 pixels, limit is 4"
        );
        assert_eq!(Error::Unavailable("gone".into()).to_string(), "gone");
    }
}
