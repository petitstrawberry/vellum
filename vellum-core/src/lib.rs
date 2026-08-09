//! Vellum's pure-Rust document loading and rasterization core.
//!
//! The core deliberately has no dependency on Scarlet or on a windowing
//! system. It decodes common raster image formats with `image` and rasterizes
//! PDF pages with `hayro`, so the same document model can be used by the
//! ScarletUI frontends and headless tests.

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use hayro::hayro_interpret::{InterpreterSettings, hayro_syntax::Pdf};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings, render};

// Render at 144 DPI for 72-point PDF coordinates. This keeps text legible
// after the page is fitted to a window while the bitmap-size guards below
// prevent pathological documents from allocating unbounded memory.
const PDF_RENDER_SCALE: f32 = 2.0;
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

type PrefetchCallback = Box<dyn FnOnce(Result<(), DocumentError>) + Send + 'static>;

struct RenderRequest {
    index: usize,
    callback: PrefetchCallback,
}

enum DocumentSource {
    Image(Arc<Bitmap>),
    Pdf {
        pdf: Arc<Pdf>,
        cache: Mutex<Vec<Option<Arc<Bitmap>>>>,
        errors: Mutex<Vec<Option<DocumentError>>>,
        pending: Mutex<Vec<bool>>,
        queue: Mutex<Vec<RenderRequest>>,
        worker_running: AtomicBool,
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
                    errors: Mutex::new(vec![None; page_count]),
                    pending: Mutex::new(vec![false; page_count]),
                    queue: Mutex::new(Vec::new()),
                    worker_running: AtomicBool::new(false),
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

    /// Return a page only when it has already been rendered.
    ///
    /// Unlike [`Document::page`], this method never starts a synchronous PDF
    /// render. It is intended for UI code that must remain responsive while a
    /// background render is in progress.
    pub fn cached_page(&self, index: usize) -> Result<Option<Arc<Bitmap>>, DocumentError> {
        if index >= self.page_count {
            return Err(DocumentError::new("page index is out of range"));
        }

        match &self.source {
            DocumentSource::Image(bitmap) => Ok(Some(bitmap.clone())),
            DocumentSource::Pdf { cache, errors, .. } => {
                let errors = errors
                    .lock()
                    .map_err(|_| DocumentError::new("PDF render errors are poisoned"))?;
                if let Some(Some(error)) = errors.get(index) {
                    return Err(error.clone());
                }

                let cache = cache
                    .lock()
                    .map_err(|_| DocumentError::new("PDF page cache is poisoned"))?;
                Ok(cache.get(index).and_then(Option::clone))
            }
        }
    }

    /// Request a page to be rendered in the document's background worker.
    ///
    /// Repeated requests for a cached or in-flight page are ignored. The
    /// callback is called once after a newly queued request finishes.
    pub fn prefetch<F>(self: &Arc<Self>, index: usize, callback: F)
    where
        F: FnOnce(Result<(), DocumentError>) + Send + 'static,
    {
        if index >= self.page_count {
            callback(Err(DocumentError::new("page index is out of range")));
            return;
        }

        let should_start_worker = match &self.source {
            DocumentSource::Image(_) => return,
            DocumentSource::Pdf {
                cache,
                errors,
                pending,
                queue,
                worker_running,
                ..
            } => {
                let mut pending = match pending.lock() {
                    Ok(pending) => pending,
                    Err(_) => {
                        callback(Err(DocumentError::new("PDF render state is poisoned")));
                        return;
                    }
                };

                if pending.get(index).copied().unwrap_or(false) {
                    return;
                }

                let cache = match cache.lock() {
                    Ok(cache) => cache,
                    Err(_) => {
                        callback(Err(DocumentError::new("PDF page cache is poisoned")));
                        return;
                    }
                };

                if cache.get(index).and_then(Option::as_ref).is_some() {
                    return;
                }

                let errors = match errors.lock() {
                    Ok(errors) => errors,
                    Err(_) => {
                        callback(Err(DocumentError::new("PDF render errors are poisoned")));
                        return;
                    }
                };
                if let Some(Some(error)) = errors.get(index) {
                    callback(Err(error.clone()));
                    return;
                }

                let Some(slot) = pending.get_mut(index) else {
                    callback(Err(DocumentError::new("page index is out of range")));
                    return;
                };
                *slot = true;
                drop(errors);
                drop(cache);
                drop(pending);

                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(_) => {
                        let result = Err(DocumentError::new("PDF render queue is poisoned"));
                        self.finish_prefetch(index, &result);
                        callback(result);
                        return;
                    }
                };
                queue.push(RenderRequest {
                    index,
                    callback: Box::new(callback),
                });
                drop(queue);

                worker_running
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }
        };

        if should_start_worker {
            let document = Arc::clone(self);
            thread::spawn(move || document.render_queue_worker());
        }
    }

    /// Return a rasterized page, rendering and caching PDF pages on demand.
    pub fn page(&self, index: usize) -> Result<Arc<Bitmap>, DocumentError> {
        if index >= self.page_count {
            return Err(DocumentError::new("page index is out of range"));
        }

        self.render_page_now(index)
    }

    fn render_page_now(&self, index: usize) -> Result<Arc<Bitmap>, DocumentError> {
        if index >= self.page_count {
            return Err(DocumentError::new("page index is out of range"));
        }

        match &self.source {
            DocumentSource::Image(bitmap) => Ok(bitmap.clone()),
            DocumentSource::Pdf {
                pdf, cache, errors, ..
            } => {
                {
                    let errors = errors
                        .lock()
                        .map_err(|_| DocumentError::new("PDF render errors are poisoned"))?;
                    if let Some(Some(error)) = errors.get(index) {
                        return Err(error.clone());
                    }
                }

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
                if let Ok(mut errors) = errors.lock()
                    && let Some(slot) = errors.get_mut(index)
                {
                    *slot = None;
                }
                Ok(bitmap)
            }
        }
    }

    fn finish_prefetch(&self, index: usize, result: &Result<(), DocumentError>) {
        let DocumentSource::Pdf {
            errors, pending, ..
        } = &self.source
        else {
            return;
        };

        if let Ok(mut pending) = pending.lock()
            && let Some(slot) = pending.get_mut(index)
        {
            *slot = false;
        }
        if let Err(error) = result
            && let Ok(mut errors) = errors.lock()
            && let Some(slot) = errors.get_mut(index)
        {
            *slot = Some(error.clone());
        }
    }

    fn render_queue_worker(self: Arc<Self>) {
        loop {
            let request = match &self.source {
                DocumentSource::Pdf { queue, .. } => {
                    queue.lock().ok().and_then(|mut queue| queue.pop())
                }
                DocumentSource::Image(_) => None,
            };

            let Some(request) = request else {
                let DocumentSource::Pdf {
                    queue,
                    worker_running,
                    ..
                } = &self.source
                else {
                    return;
                };

                if worker_running
                    .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    continue;
                }

                let has_queued_work = queue.lock().map(|queue| !queue.is_empty()).unwrap_or(false);
                if has_queued_work
                    && worker_running
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                return;
            };

            let result = self.render_page_now(request.index).map(|_| ());
            self.finish_prefetch(request.index, &result);
            (request.callback)(result);
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
