use std::fmt::{Debug, Error, Formatter};
use std::ops::{Deref, DerefMut};

use anyhow::{Result, anyhow};
use image::{DynamicImage, RgbaImage};
use libc::c_int;
use libwebp_sys::WebPEncodingError::VP8_ENC_OK;
use libwebp_sys::WebPPreset::WEBP_PRESET_DEFAULT;
use libwebp_sys::*;
pub use libwebp_sys::WebPConfig;


/// Inits the global encoder config.
///
///     - quality:
///         This parameter is the amount of effort put into the
///         compression: 0 is the fastest but gives larger
///         files compared to the slowest, but best, 100.
///
///     - method:
///         The quality / speed trade-off (0=fast, 6=slower-better)
///
///     - multi_threading:
///         If the system should to attempt to use in multi-threaded encoding.
pub fn config(lossless: bool, quality: f32, method: i32, multi_threading: bool) -> WebPConfig {
    WebPConfig {
        lossless: if lossless { 1 } else { 0 },
        quality,
        method,
        image_hint: WebPImageHint::WEBP_HINT_DEFAULT,
        target_size: 0,
        target_PSNR: 0.0,
        segments: 4,
        sns_strength: 0,
        filter_strength: 0,
        filter_sharpness: 0,
        filter_type: 0,
        autofilter: 0,
        alpha_compression: 1,
        alpha_filtering: 1,
        alpha_quality: 100,
        pass: 5,
        show_compressed: 1,
        preprocessing: 0,
        partitions: 0,
        partition_limit: 0,
        emulate_jpeg_size: 0,
        thread_level: if multi_threading { 1 } else { 0 },
        low_memory: 0,
        near_lossless: 100,
        exact: 0,
        use_delta_palette: 0,
        use_sharp_yuv: 0,
        qmin: 0,
        qmax: 100,
    }
}

/// Picture is uninitialized.
pub fn empty_webp_picture() -> WebPPicture {
    WebPPicture {
        use_argb: 1,

        // YUV input
        colorspace: WebPEncCSP::WEBP_YUV420,
        width: 0,
        height: 0,
        y: std::ptr::null_mut(),
        u: std::ptr::null_mut(),
        v: std::ptr::null_mut(),
        y_stride: 0,
        uv_stride: 0,
        a: std::ptr::null_mut(),
        a_stride: 0,
        pad1: [0, 0],

        // ARGB input
        argb: std::ptr::null_mut(),
        argb_stride: 0,
        pad2: [0, 0, 0],

        // OUTPUT
        writer: None,
        custom_ptr: std::ptr::null_mut(),
        extra_info_type: 0,
        extra_info: std::ptr::null_mut(),

        // STATS AND REPORTS
        stats: std::ptr::null_mut(),
        error_code: VP8_ENC_OK,
        progress_hook: None,
        user_data: std::ptr::null_mut(),

        // padding for later use
        pad3: [0, 0, 0],

        // Unused for now
        pad4: std::ptr::null_mut(),
        pad5: std::ptr::null_mut(),

        // padding for later use
        pad6: [0, 0, 0, 0, 0, 0, 0, 0],

        // PRIVATE FIELDS
        memory_: std::ptr::null_mut(),
        memory_argb_: std::ptr::null_mut(),
        pad7: [std::ptr::null_mut(), std::ptr::null_mut()],
    }
}

#[derive(Clone, Debug)]
pub enum PixelLayout {
    RGB,
    RGBA,
    BGR,
    BGRA,
    Other(RgbaImage),
}

pub struct Encoder<'a> {
    cfg: WebPConfig,
    layout: PixelLayout,
    image: &'a [u8],
    width: u32,
    height: u32,
}

impl<'a> Encoder<'a> {
    /// Creates a new encoder from the given image.
    pub fn from_image(cfg: WebPConfig, image: &'a DynamicImage) -> Self {
        match image {
            DynamicImage::ImageRgb8(image) => {
                Self::from_rgb(cfg, image.as_ref(), image.width(), image.height())
            },
            DynamicImage::ImageRgba8(image) => {
                Self::from_rgba(cfg,image.as_ref(), image.width(), image.height())
            },
            other => {
                let image = other.to_rgba8();
                Self::from_other(cfg,other.as_bytes(), other.width(), other.height(), image)
            },
        }
    }

    /// Creates a new encoder from the given image data in the RGB pixel layout.
    pub fn from_rgb(cfg: WebPConfig, image: &'a [u8], width: u32, height: u32) -> Self {
        Self {
            cfg,
            image,
            width,
            height,
            layout: PixelLayout::RGB,
        }
    }

    /// Creates a new encoder from the given image data in the RGBA pixel layout.
    pub fn from_rgba(cfg: WebPConfig, image: &'a [u8], width: u32, height: u32) -> Self {
        Self {
            cfg,
            image,
            width,
            height,
            layout: PixelLayout::RGBA,
        }
    }

    /// Creates a new encoder from the given image data in the Other layout,
    /// this creates a copy of the data to convert it to RGBA.
    pub fn from_other(cfg: WebPConfig, image: &'a [u8], width: u32, height: u32, other: RgbaImage) -> Self {
        Self {
            cfg,
            image,
            width,
            height,
            layout: PixelLayout::Other(other),
        }
    }

    /// Encode the image with the given global config.
    pub fn encode(self) -> Result<WebPMemory> {
        let (img, layout) = if let PixelLayout::Other(img) = &self.layout {
            (img.as_ref(), &PixelLayout::RGBA)
        } else {
            (self.image.as_ref(), &self.layout)
        };

        unsafe { encode(self.cfg, img, layout, self.width, self.height) }
    }
}

macro_rules! check_ok {
    ( $e:expr, $msg:expr ) => {{
        if $e == 0 {
            return Err(anyhow!("{}", $msg));
        }
    }};
}

unsafe fn encode(cfg: WebPConfig, image: &[u8], layout: &PixelLayout, width: u32, height: u32) -> Result<WebPMemory> {
    let picture = empty_webp_picture();
    let writer = WebPMemoryWriter {
        mem: std::ptr::null_mut::<u8>(),
        size: 0,
        max_size: 0,
        pad: [0],
    };

    let cfg_ptr = Box::into_raw(Box::from(cfg));
    let picture_ptr = Box::into_raw(Box::from(picture));
    let writer_ptr = Box::into_raw(Box::from(writer));

    let ok = WebPConfigInitInternal(
        cfg_ptr,
        WEBP_PRESET_DEFAULT,
        cfg.quality,
        WEBP_ENCODER_ABI_VERSION as c_int,
    );
    check_ok!(ok, "config init failed");

    let ok = WebPPictureInitInternal(picture_ptr, WEBP_ENCODER_ABI_VERSION as c_int);
    check_ok!(ok, "picture init failed");

    (*picture_ptr).use_argb = cfg.lossless;
    (*cfg_ptr).lossless = cfg.lossless;
    (*cfg_ptr).method = cfg.method;
    (*cfg_ptr).thread_level = cfg.thread_level;

    let width = width as _;
    let height = height as _;

    (*picture_ptr).width = width;
    (*picture_ptr).height = height;
    (*picture_ptr).writer = WebPWriterFunction::Some(WebPMemoryWrite);
    (*picture_ptr).custom_ptr = writer_ptr as *mut _;
    WebPMemoryWriterInit(writer_ptr);

    let ok = match layout {
        PixelLayout::RGB => {
            let stride = width * 3;
            WebPPictureImportRGB(picture_ptr, image.as_ptr(), stride)
        },
        PixelLayout::RGBA => {
            let stride = width * 4;
            WebPPictureImportRGBA(picture_ptr, image.as_ptr(), stride)
        },
        PixelLayout::BGR => {
            let stride = width * 3;
            WebPPictureImportBGR(picture_ptr, image.as_ptr(), stride)
        },
        PixelLayout::BGRA => {
            let stride = width * 4;
            WebPPictureImportBGRA(picture_ptr, image.as_ptr(), stride)
        },
        _ => unreachable!(),
    };
    check_ok!(ok, "failed to import image");

    let ok = WebPEncode(cfg_ptr, picture_ptr);
    WebPPictureFree(picture_ptr);
    if ok == 0 {
        WebPMemoryWriterClear(writer_ptr);
        return Err(anyhow!(
            "memory error. libwebp error code: {:?}",
            (*picture_ptr).error_code
        ))
    }

    Ok(WebPMemory((*writer_ptr).mem, (*writer_ptr).size))
}

/// This struct represents a safe wrapper around memory owned by libwebp.
/// Its data contents can be accessed through the Deref and DerefMut traits.
pub struct WebPMemory(pub(crate) *mut u8, pub(crate) usize);

impl Debug for WebPMemory {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        f.debug_struct("WebpMemory").finish()
    }
}

impl Drop for WebPMemory {
    fn drop(&mut self) {
        unsafe { WebPFree(self.0 as _) }
    }
}

impl Deref for WebPMemory {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.0, self.1) }
    }
}

impl DerefMut for WebPMemory {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { std::slice::from_raw_parts_mut(self.0, self.1) }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::write;

    use super::*;

    fn ensure_global() {
        config(true, 50.0, 6, true)
    }

    #[test]
    fn test_basic_sample_1() {
        let image = image::open("./test_samples/news.png").expect("load image");
        ensure_global();

        let encoder = Encoder::from_image(&image);
        let start = std::time::Instant::now();
        let memory = encoder.encode();
        println!("{:?}", start.elapsed());
        let buffer = memory.as_ref();
        write("./news.webp", buffer).expect("write image");
    }

    #[test]
    fn test_basic_sample_2() {
        let image = image::open("./test_samples/release.png").expect("load image");
        ensure_global();

        let encoder = Encoder::from_image(&image);
        let start = std::time::Instant::now();
        let memory = encoder.encode();
        println!("{:?}", start.elapsed());
        let buffer = memory.as_ref();

        write("./release.webp", buffer).expect("write image");
    }
}
