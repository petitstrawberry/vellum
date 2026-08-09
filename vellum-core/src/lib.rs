//! Vellum's pure-Rust document loading and rasterization core.
//!
//! The core deliberately has no dependency on Scarlet or on a windowing
//! system. It decodes common raster image formats with `image` and rasterizes
//! PDF pages with `hayro`, so the same document model can be used by the
//! Scarlet frontend, the host frontend, and headless tests.

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hayro::hayro_interpret::{InterpreterSettings, hayro_syntax::Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings, render};

const PDF_RENDER_SCALE: f32 = 1.25;
const PDF_MAX_DIMENSION: f32 = 4096.0;
const MAX_BITMAP_PIXELS: u64 = 64 * 1024 * 1024;

/// A decoded, row-major RGBA8 bitmap.
#[derive(Clone, Debug)]
pub struct Bitmap {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Bitmap {
    /// Construct a bitmap from tightly packed RGBA8 pixels.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, DocumentError> {
        let pixel_count = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| DocumentError::new("bitmap dimensions overflow"))?;
        if pixel_count == 0 {
            return Err(DocumentError::new("bitmap dimensions must be non-zero"));
        }
        if pixel_count > MAX_BITMAP_PIXELS {
            return Err(DocumentError::new("bitmap is too large to display"));
        }
        let expected_len = pixel_count
            .checked_mul(4)
            .and_then(|size| usize::try_from(size).ok())
            .ok_or_else(|| DocumentError::new("bitmap buffer size overflow"))?;
        if rgba.len() != expected_len {
            return Err(DocumentError::new("bitmap buffer has an invalid length"));
        }

        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Return the bitmap width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Return the bitmap height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Return the packed RGBA8 pixels.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Convert the bitmap to the BGRA words expected by ScarletUI.
    ///
    /// The returned words are in native little-endian byte order, with byte 0
    /// blue, byte 1 green, byte 2 red, and byte 3 alpha.
    pub fn to_bgra_words(&self) -> Vec<u32> {
        self.rgba
            .chunks_exact(4)
            .map(|pixel| {
                (u32::from(pixel[3]) << 24)
                    | (u32::from(pixel[0]) << 16)
                    | (u32::from(pixel[1]) << 8)
                    | u32::from(pixel[2])
            })
            .collect()
    }

    /// Save the bitmap as a PNG file.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<(), DocumentError> {
        let image = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| DocumentError::new("could not construct PNG image buffer"))?;
        image
            .save_with_format(path, image::ImageFormat::Png)
            .map_err(|error| DocumentError::new(format!("could not save PNG: {error}")))
    }
}

/// Errors returned while opening or rasterizing a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentError(String);

impl DocumentError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl Display for DocumentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DocumentError {}

enum DocumentSource {
    Image(Arc<Bitmap>),
    Pdf {
        pdf: Arc<Pdf>,
        cache: Mutex<Vec<Option<Arc<Bitmap>>>>,
    },
}

/// A document that can be displayed one page at a time.
pub struct Document {
    path: PathBuf,
    title: String,
    page_count: usize,
    source: DocumentSource,
}

impl Document {
    /// Open an image or PDF document from a filesystem path.
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, DocumentError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|error| DocumentError::new(format!("could not read document: {error}")))?;
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| String::from("Untitled"));

        if is_pdf(path, &bytes) {
            let pdf = Pdf::new(bytes)
                .map_err(|error| DocumentError::new(format!("could not parse PDF: {error:?}")))?;
            let page_count = pdf.pages().len();
            if page_count == 0 {
                return Err(DocumentError::new("PDF has no pages"));
            }
            return Ok(Arc::new(Self {
                path: path.to_path_buf(),
                title,
                page_count,
                source: DocumentSource::Pdf {
                    pdf: Arc::new(pdf),
                    cache: Mutex::new(vec![None; page_count]),
                },
            }));
        }

        let decoded = image::load_from_memory(&bytes)
            .map_err(|error| DocumentError::new(format!("could not decode image: {error}")))?;
        let rgba = decoded.into_rgba8();
        let bitmap = Arc::new(Bitmap::from_rgba(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
        )?);

        Ok(Arc::new(Self {
            path: path.to_path_buf(),
            title,
            page_count: 1,
            source: DocumentSource::Image(bitmap),
        }))
    }

    /// Return the original document path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the display title, normally the filename.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the number of pages. Raster images have one page.
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Return a rasterized page, rendering and caching PDF pages on demand.
    pub fn page(&self, index: usize) -> Result<Arc<Bitmap>, DocumentError> {
        if index >= self.page_count {
            return Err(DocumentError::new("page index is out of range"));
        }

        match &self.source {
            DocumentSource::Image(bitmap) => Ok(bitmap.clone()),
            DocumentSource::Pdf { pdf, cache } => {
                {
                    let cache = cache
                        .lock()
                        .map_err(|_| DocumentError::new("PDF page cache is poisoned"))?;
                    if let Some(Some(bitmap)) = cache.get(index) {
                        return Ok(bitmap.clone());
                    }
                }

                let bitmap = Arc::new(render_pdf_page(pdf, index)?);
                let mut cache = cache
                    .lock()
                    .map_err(|_| DocumentError::new("PDF page cache is poisoned"))?;
                if let Some(slot) = cache.get_mut(index) {
                    *slot = Some(bitmap.clone());
                }
                Ok(bitmap)
            }
        }
    }
}

fn is_pdf(path: &Path, bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn render_pdf_page(pdf: &Pdf, index: usize) -> Result<Bitmap, DocumentError> {
    let page = pdf
        .pages()
        .get(index)
        .ok_or_else(|| DocumentError::new("page index is out of range"))?;
    let (width, height) = page.render_dimensions();
    let scale = PDF_RENDER_SCALE
        .min(PDF_MAX_DIMENSION / width.max(1.0))
        .min(PDF_MAX_DIMENSION / height.max(1.0))
        .max(0.01);
    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
            ..RenderSettings::default()
        },
    );

    Bitmap::from_rgba(
        u32::from(pixmap.width()),
        u32::from(pixmap.height()),
        pixmap.data_as_u8_slice().to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::Bitmap;

    #[test]
    fn converts_rgba_to_scarlet_bgra_words() {
        let bitmap = Bitmap::from_rgba(2, 1, vec![10, 20, 30, 40, 50, 60, 70, 80])
            .expect("bitmap should be valid");

        assert_eq!(bitmap.to_bgra_words(), vec![0x280a141e, 0x50323c46]);
    }

    #[test]
    fn rejects_invalid_pixel_buffer_length() {
        assert!(Bitmap::from_rgba(1, 1, vec![0, 1, 2]).is_err());
    }
}
