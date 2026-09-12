//! Image intake and bounded decoded-image lifetime for the native editor.
//!
//! The routing types are intentionally independent of the Markdown donor.  The
//! payload extraction follows the donor's image-first classification and the
//! GPUI 0.2.2 `RetainAllImageCache` loading protocol; only the document model
//! decides when a structural `InsertImage` transaction is committed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use base64::Engine as _;
use futures::FutureExt;
use gpui::{
    App, AppContext, ClipboardEntry, ClipboardItem, Context, Entity, Image, ImageCache,
    ImageCacheError, ImageCacheItem, ImageFormat, RenderImage, Resource, WeakEntity, Window, hash,
};
use image::{AnimationDecoder, ImageBuffer, codecs::gif::GifDecoder, imageops::FilterType};
use smallvec::SmallVec;

/// A resource's source stays separate from its presentation metadata. Bytes
/// received from a clipboard are already process-owned and short-lived, while
/// a Finder/picker file remains an opened descriptor until the repository
/// stages it with a fixed-size copy buffer. Neither `Document` nor
/// `EditorCore` retains either payload.
#[derive(Debug)]
pub(crate) enum ResourceSource {
    Bytes(Vec<u8>),
    File { file: File, size: usize },
}

impl ResourceSource {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::File { size, .. } => *size,
        }
    }
}

/// A file/clipboard resource normalized before it crosses into the durable
/// note session.
#[derive(Debug)]
pub struct ResourceImport {
    source: ResourceSource,
    pub title: String,
    pub mime: String,
    pub extension: String,
    pub kind: ResourceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Image {
        format: ImageFormat,
        natural_size: (u32, u32),
    },
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceImportError {
    UnsupportedFile,
    UnsafeFile,
    TooLarge,
    InvalidImage,
    Io(String),
}

impl fmt::Display for ResourceImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFile => f.write_str("不支持的资源格式"),
            Self::UnsafeFile => f.write_str("资源文件路径不安全"),
            Self::TooLarge => f.write_str("资源文件超过允许大小"),
            Self::InvalidImage => f.write_str("图片内容无法安全解码"),
            Self::Io(message) => write!(f, "无法读取资源文件：{message}"),
        }
    }
}

impl std::error::Error for ResourceImportError {}

impl ResourceImport {
    pub fn from_path(path: &Path) -> Result<Self, ResourceImportError> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| ResourceImportError::Io(error.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ResourceImportError::UnsafeFile);
        }
        if metadata.len() == 0 {
            return Err(ResourceImportError::UnsupportedFile);
        }
        if metadata.len() > app_lite_core::MAX_RESOURCE_BYTES as u64 {
            return Err(ResourceImportError::TooLarge);
        }
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .ok_or(ResourceImportError::UnsafeFile)?
            .to_owned();
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|extension| valid_extension(extension))
            .ok_or(ResourceImportError::UnsupportedFile)?;
        let mime = mime_for_extension(&extension);
        let mut file = open_checked_resource_source(path, &metadata)?;
        let size = usize::try_from(metadata.len()).map_err(|_| ResourceImportError::TooLarge)?;
        let kind = if let Some(format) = image_format_for_mime(&mime) {
            if size > app_lite_core::MAX_IMAGE_BYTES {
                return Err(ResourceImportError::TooLarge);
            }
            let inspection = file
                .try_clone()
                .map_err(|error| ResourceImportError::Io(error.to_string()))?;
            let (_actual, natural_size) = inspect_persisted_image(inspection, &mime)?;
            // `try_clone` shares the descriptor's file offset on Unix. Reset
            // the source before handing it to the streaming repository.
            file.seek(SeekFrom::Start(0))
                .map_err(|error| ResourceImportError::Io(error.to_string()))?;
            ResourceKind::Image {
                format,
                natural_size,
            }
        } else {
            ResourceKind::Attachment
        };
        Ok(Self {
            source: ResourceSource::File { file, size },
            title,
            mime,
            extension,
            kind,
        })
    }

    pub fn from_image_payload(payload: ImagePayload) -> Result<Self, ResourceImportError> {
        let extension = image_extension(payload.format).to_owned();
        let title = payload.name.unwrap_or_else(|| format!("图片.{extension}"));
        Self::from_bytes_inner(
            payload.bytes,
            title,
            mime_for_image_format(payload.format).to_owned(),
            extension,
        )
    }

    pub fn from_bytes(
        bytes: Vec<u8>,
        title: impl Into<String>,
        mime: impl Into<String>,
        extension: impl Into<String>,
    ) -> Result<Self, ResourceImportError> {
        Self::from_bytes_inner(bytes, title.into(), mime.into(), extension.into())
    }

    fn from_bytes_inner(
        bytes: Vec<u8>,
        title: String,
        mime: String,
        extension: String,
    ) -> Result<Self, ResourceImportError> {
        if bytes.is_empty() || !valid_extension(&extension) || title.trim().is_empty() {
            return Err(ResourceImportError::UnsupportedFile);
        }
        if bytes.len() > app_lite_core::MAX_RESOURCE_BYTES {
            return Err(ResourceImportError::TooLarge);
        }
        let kind = if let Some(format) = image_format_for_mime(&mime) {
            if bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
                return Err(ResourceImportError::TooLarge);
            }
            let natural_size =
                image_dimensions(&bytes, format).ok_or(ResourceImportError::InvalidImage)?;
            ResourceKind::Image {
                format,
                natural_size,
            }
        } else {
            ResourceKind::Attachment
        };
        Ok(Self {
            source: ResourceSource::Bytes(bytes),
            title,
            mime,
            extension,
            kind,
        })
    }

    pub const fn is_image(&self) -> bool {
        matches!(self.kind, ResourceKind::Image { .. })
    }

    pub(crate) fn into_parts(self) -> (ResourceSource, String, String, String, ResourceKind) {
        (
            self.source,
            self.title,
            self.mime,
            self.extension,
            self.kind,
        )
    }
}

/// Bind a regular local source through an opened descriptor before it crosses
/// the resource-store boundary. The initial `symlink_metadata` validates the
/// UI-visible path; `O_NOFOLLOW` and the post-open identity check make the
/// descriptor authoritative against a replacement between that validation and
/// the open. The repository never receives this external pathname.
fn open_checked_resource_source(
    path: &Path,
    expected: &std::fs::Metadata,
) -> Result<File, ResourceImportError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|error| ResourceImportError::Io(error.to_string()))?;
    let opened = file
        .metadata()
        .map_err(|error| ResourceImportError::Io(error.to_string()))?;
    if !opened.is_file() || opened.len() != expected.len() {
        return Err(ResourceImportError::UnsafeFile);
    }
    #[cfg(unix)]
    if opened.dev() != expected.dev() || opened.ino() != expected.ino() {
        return Err(ResourceImportError::UnsafeFile);
    }
    Ok(file)
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension.len() <= 16
        && extension
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn mime_for_extension(extension: &str) -> String {
    match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn mime_for_image_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::Webp => "image/webp",
        ImageFormat::Svg => "image/svg+xml",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Tiff => "image/tiff",
    }
}

fn image_format_for_mime(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/svg+xml" => ImageFormat::Svg,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

fn image_dimensions(bytes: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    if format == ImageFormat::Svg {
        let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
        let size = tree.size();
        return Some((size.width().ceil() as u32, size.height().ceil() as u32));
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if sniffed_raster_format(reader.format()?) != Some(format) {
        return None;
    }
    reader.into_dimensions().ok()
}

/// Inspect one already-open, descriptor-safe resource stream. Raster formats
/// stay fully streaming; SVG's parser accepts a byte slice, so it is the one
/// bounded exception and is released before the next document node is
/// inspected. Callers must not turn a whole note's images into `Vec`s first.
pub fn inspect_persisted_image(
    file: File,
    mime: &str,
) -> Result<(ImageFormat, (u32, u32)), ResourceImportError> {
    let format = image_format_for_mime(mime).ok_or(ResourceImportError::InvalidImage)?;
    if format == ImageFormat::Svg {
        let mut limited = file.take(app_lite_core::MAX_IMAGE_BYTES as u64 + 1);
        let mut bytes = Vec::new();
        limited
            .read_to_end(&mut bytes)
            .map_err(|error| ResourceImportError::Io(error.to_string()))?;
        if bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
            return Err(ResourceImportError::TooLarge);
        }
        let dimensions =
            image_dimensions(&bytes, format).ok_or(ResourceImportError::InvalidImage)?;
        return Ok((format, dimensions));
    }
    let reader = image::ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|_| ResourceImportError::InvalidImage)?;
    if sniffed_raster_format(reader.format().ok_or(ResourceImportError::InvalidImage)?)
        != Some(format)
    {
        return Err(ResourceImportError::InvalidImage);
    }
    let dimensions = reader
        .into_dimensions()
        .map_err(|_| ResourceImportError::InvalidImage)?;
    Ok((format, dimensions))
}

/// File names and advertised MIME type are untrusted intake hints. The image
/// cache ultimately decodes the managed path based on its extension, so do
/// not persist an image unless the actual raster signature agrees with the
/// declared native-editor format.
fn sniffed_raster_format(format: image::ImageFormat) -> Option<ImageFormat> {
    Some(match format {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::WebP => ImageFormat::Webp,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        image::ImageFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    })
}

pub const DECODED_IMAGE_CACHE_BUDGET: usize = 48 * 1024 * 1024;
const DEFAULT_PROXY_MAX_EDGE: u32 = 1600;
const PROXY_EDGE_TIER: u32 = 64;
const CONSERVATIVE_PROXY_RESERVATION: usize = 4 * 1024 * 1024;

/// Return the minimum proxy edge that can cover a viewport-sized image at the
/// display's device-pixel density.  GPUI dimensions are logical pixels, while
/// `RenderImage` stores device pixels, so using a fixed edge here would blur
/// images on Retina displays.
pub(crate) fn proxy_max_edge_for_viewport(viewport_width: f32, scale_factor: f32) -> u32 {
    if !viewport_width.is_finite()
        || viewport_width <= 0.0
        || !scale_factor.is_finite()
        || scale_factor <= 0.0
    {
        return 1;
    }
    (f64::from(viewport_width) * f64::from(scale_factor))
        .ceil()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

fn proxy_reservation_bytes(max_edge: u32) -> usize {
    (max_edge as usize)
        .checked_mul(max_edge as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(usize::MAX)
}

fn quantize_proxy_edge(max_edge: u32) -> u32 {
    let max_edge = max_edge.max(1);
    let tiers = max_edge / PROXY_EDGE_TIER + u32::from(max_edge % PROXY_EDGE_TIER != 0);
    tiers.saturating_mul(PROXY_EDGE_TIER)
}

fn quantize_proxy_edge_with_natural_max(max_edge: u32, natural_max_edge: Option<u32>) -> u32 {
    let natural_max_edge = natural_max_edge.filter(|edge| *edge > 0);
    let capped_edge = natural_max_edge.map_or(max_edge, |natural| max_edge.min(natural));
    let quantized_edge = quantize_proxy_edge(capped_edge);
    natural_max_edge.map_or(quantized_edge, |natural| quantized_edge.min(natural))
}

#[cfg(target_os = "macos")]
mod mac_pressure {
    #[cfg(test)]
    use std::cell::RefCell;
    use std::path::Path;

    const LARGE_RESOURCE_BYTES: u64 = 4 * 1024 * 1024;

    #[cfg(test)]
    type TestHook = Box<dyn Fn() + Send + Sync + 'static>;

    #[cfg(test)]
    thread_local! {
        static TEST_HOOK: RefCell<Option<TestHook>> = const { RefCell::new(None) };
    }

    #[link(name = "System")]
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    /// Return unused large malloc-zone pages after a large ImageIO decode has
    /// released its source/thumbnail objects.  This is deliberately called at
    /// the resource-load boundary, never from paint or cache-hit paths.
    pub fn relieve_for_path(path: &Path, decoded_bytes: usize) {
        let source_bytes = std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        if source_bytes < LARGE_RESOURCE_BYTES && (decoded_bytes as u64) < LARGE_RESOURCE_BYTES {
            return;
        }

        #[cfg(test)]
        let intercepted = TEST_HOOK.with(|slot| {
            let hook = slot.borrow();
            if let Some(hook) = hook.as_ref() {
                hook();
                true
            } else {
                false
            }
        });
        #[cfg(test)]
        if intercepted {
            return;
        }

        // SAFETY: libSystem accepts a null zone to relieve all malloc zones;
        // this call only asks the allocator to return currently-unused pages.
        unsafe {
            let _ = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    /// Return allocator pages after the cache has dropped decoded proxies.
    /// This is deliberately tied to an eviction boundary, not every paint.
    pub fn relieve_unused() {
        unsafe {
            let _ = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    #[cfg(test)]
    pub fn set_test_hook(hook: Option<TestHook>) {
        TEST_HOOK.with(|slot| *slot.borrow_mut() = hook);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePayload {
    pub format: ImageFormat,
    pub bytes: Vec<u8>,
    pub name: Option<String>,
}

impl ImagePayload {
    pub fn new(format: ImageFormat, bytes: Vec<u8>) -> Self {
        Self {
            format,
            bytes,
            name: None,
        }
    }

    pub fn from_image(image: Image) -> Self {
        Self::new(image.format, image.bytes)
    }
}

/// An HTML data URI stays encoded until the retained resource worker runs.
/// The GPUI callback may retain only this bounded descriptor; it never creates
/// a second decoded image buffer or decodes untrusted base64 inline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedImagePayload {
    format: ImageFormat,
    encoded: String,
}

impl EncodedImagePayload {
    fn new(format: ImageFormat, encoded: String) -> Self {
        Self { format, encoded }
    }

    pub(crate) fn decode_bounded(self) -> Result<ImagePayload, ResourceImportError> {
        if self.encoded.is_empty() || self.encoded.len() > MAX_DATA_URI_ENCODED_BYTES {
            return Err(ResourceImportError::TooLarge);
        }
        let decoded_upper_bound = self
            .encoded
            .len()
            .checked_div(4)
            .and_then(|quads| quads.checked_mul(3))
            .ok_or(ResourceImportError::TooLarge)?;
        if decoded_upper_bound > app_lite_core::MAX_IMAGE_BYTES {
            return Err(ResourceImportError::TooLarge);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.encoded)
            .map_err(|_| ResourceImportError::InvalidImage)?;
        if bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
            return Err(ResourceImportError::TooLarge);
        }
        Ok(ImagePayload::new(self.format, bytes))
    }
}

const MAX_CLIPBOARD_IMAGE_CANDIDATE_BYTES: usize = 3 * app_lite_core::MAX_IMAGE_BYTES;
const MAX_CLIPBOARD_PATH_CANDIDATES: usize = 8;
const MAX_DATA_URI_ENCODED_BYTES: usize = ((app_lite_core::MAX_IMAGE_BYTES + 2) / 3) * 4;
const MAX_HTML_IMAGE_DESCRIPTOR_BYTES: usize = MAX_DATA_URI_ENCODED_BYTES + 4096;

/// Normalized clipboard representation.  macOS extraction fills `images`
/// before `text`, because GPUI 0.2.2 itself returns public.utf8-plain-text
/// before image UTTypes on that platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub images: Vec<ImagePayload>,
    pub file_urls: Vec<PathBuf>,
    pub temporary_files: Vec<PathBuf>,
    pub html: Option<String>,
    pub rich_text: Option<String>,
    pub text: Option<String>,
    /// Native AppKit saw image representations, but none could pass the
    /// bounded ownership boundary. This is not the same as a missing native
    /// representation: it blocks fallback to screenshot placeholder text.
    pub native_image_rejected: bool,
}

impl ClipboardPayload {
    pub fn fixture_with_png_and_text(text: impl Into<String>) -> Self {
        Self {
            images: vec![ImagePayload::new(ImageFormat::Png, fixture_png_bytes())],
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn from_gpui(item: ClipboardItem) -> Self {
        let mut payload = Self::default();
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    payload.images.push(ImagePayload::from_image(image))
                }
                ClipboardEntry::String(string) => payload.text = Some(string.text().to_owned()),
            }
        }
        payload.images = bounded_image_candidates(std::mem::take(&mut payload.images));
        payload
    }

    pub fn with_image(mut self, image: ImagePayload) -> Self {
        self.images.push(image);
        self
    }

    pub fn with_file_url(mut self, path: impl Into<PathBuf>) -> Self {
        self.file_urls.push(path.into());
        self
    }

    pub fn image(&self) -> Option<&ImagePayload> {
        self.images.first()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteIntent {
    Image {
        payload: ImagePayload,
    },
    ImageCandidates {
        candidates: Vec<ImagePayload>,
    },
    EncodedImage {
        payload: EncodedImagePayload,
    },
    File {
        path: PathBuf,
        cleanup: bool,
    },
    FileCandidates {
        paths: Vec<PathBuf>,
        cleanup_paths: Vec<PathBuf>,
    },
    Text {
        text: String,
    },
    Unsupported,
}

/// Adapted from donor `components/block/interactions.rs`: classify image
/// payloads before text and never insert an image placeholder string.
pub fn classify_clipboard(payload: ClipboardPayload) -> PasteIntent {
    let ClipboardPayload {
        images,
        file_urls,
        temporary_files,
        html,
        rich_text,
        text,
        native_image_rejected,
    } = payload;
    if native_image_rejected {
        return PasteIntent::Unsupported;
    }
    if !images.is_empty() {
        let candidates = bounded_image_candidates(images);
        return if candidates.is_empty() {
            PasteIntent::Unsupported
        } else if candidates.len() == 1 {
            // Move the sole bounded payload through the callback boundary.
            // Cloning it here would briefly double a 10 MiB clipboard image
            // before the retained staging worker owns it.
            PasteIntent::Image {
                payload: candidates.into_iter().next().expect("one candidate"),
            }
        } else {
            PasteIntent::ImageCandidates { candidates }
        };
    }
    // The actual importer validates the selected file. Do not discard a PDF
    // or other supported attachment here merely because it is not an inline
    // image; paste and Finder drop share one resource transaction.
    let paths = file_urls
        .into_iter()
        .take(MAX_CLIPBOARD_PATH_CANDIDATES)
        .collect::<Vec<_>>();
    if !paths.is_empty() {
        let cleanup_paths = temporary_files
            .into_iter()
            .filter(|temporary| paths.iter().any(|path| path == temporary))
            .collect::<Vec<_>>();
        return match paths.as_slice() {
            [path] => PasteIntent::File {
                path: path.clone(),
                cleanup: cleanup_paths.iter().any(|temporary| temporary == path),
            },
            _ => PasteIntent::FileCandidates {
                paths,
                cleanup_paths,
            },
        };
    }
    if let Some(html) = html.as_deref() {
        match parse_html_image_descriptor(html) {
            HtmlImageDescriptor::DataUri(payload) => return PasteIntent::EncodedImage { payload },
            // An HTML image with an unresolved remote/file source or an
            // unsafe-sized/malformed encoded source must not become visible
            // markup or a fake PNG node.
            HtmlImageDescriptor::Rejected => return PasteIntent::Unsupported,
            HtmlImageDescriptor::NoImage => {}
        }
    }
    rich_text
        .as_ref()
        .or(text.as_ref())
        .map_or(PasteIntent::Unsupported, |text| PasteIntent::Text {
            text: text.clone(),
        })
}

/// Preserve input order while bounding only owned compressed bytes, rather
/// than arbitrarily stopping at an early representation.  This lets a later
/// small JPEG survive two corrupt PNG/TIFF candidates while keeping pasteboard
/// duplication below the explicit 30 MiB intake budget.
fn bounded_image_candidates(images: impl IntoIterator<Item = ImagePayload>) -> Vec<ImagePayload> {
    let mut total_bytes = 0usize;
    let mut candidates = Vec::new();
    for image in images {
        if image.bytes.is_empty() || image.bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
            continue;
        }
        let Some(next_total) = total_bytes.checked_add(image.bytes.len()) else {
            continue;
        };
        if next_total > MAX_CLIPBOARD_IMAGE_CANDIDATE_BYTES {
            continue;
        }
        total_bytes = next_total;
        candidates.push(image);
    }
    candidates
}

/// Testable seam for the platform paste action. Native extraction is supplied
/// by the caller, so unit tests never depend on the user's real pasteboard;
/// the ordering remains image/file-first on macOS and GPUI fallback elsewhere.
pub fn resolve_clipboard_payload(
    native: Option<ClipboardPayload>,
    gpui: Option<ClipboardPayload>,
) -> Option<ClipboardPayload> {
    let Some(native) = native else {
        return gpui;
    };
    if native.native_image_rejected {
        return Some(ClipboardPayload {
            native_image_rejected: true,
            ..ClipboardPayload::default()
        });
    }
    let Some(gpui) = gpui else {
        return Some(native);
    };
    // A pasteboard can expose a plain-text representation alongside a GPUI
    // image entry. Merge representations before classification so the
    // image-first policy is preserved instead of letting native plain text
    // suppress the image returned by GPUI.
    Some(ClipboardPayload {
        // Prefer the AppKit-owned byte copy over GPUI's duplicate image
        // representation; Finder file URLs still retain their own source.
        images: if !native.file_urls.is_empty() {
            Vec::new()
        } else if native.images.is_empty() {
            gpui.images
        } else {
            native.images
        },
        file_urls: if native.file_urls.is_empty() {
            gpui.file_urls
        } else {
            native.file_urls
        },
        temporary_files: native
            .temporary_files
            .into_iter()
            .chain(gpui.temporary_files)
            .collect(),
        html: native.html.or(gpui.html),
        rich_text: native.rich_text.or(gpui.rich_text),
        text: native.text.or(gpui.text),
        native_image_rejected: false,
    })
}

pub fn classify_drop(paths: &[PathBuf]) -> PasteIntent {
    let candidates = paths
        .iter()
        .take(MAX_CLIPBOARD_PATH_CANDIDATES)
        .cloned()
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => PasteIntent::Unsupported,
        [path] => PasteIntent::File {
            path: path.clone(),
            cleanup: false,
        },
        _ => PasteIntent::FileCandidates {
            paths: candidates,
            cleanup_paths: Vec::new(),
        },
    }
}

enum HtmlImageDescriptor {
    NoImage,
    DataUri(EncodedImagePayload),
    Rejected,
}

/// Locate one `<img>` data URI without creating a lowercase copy of the
/// complete HTML clipboard body. Every scan and copied encoded slice is
/// bounded before the worker ever invokes a base64 decoder.
fn parse_html_image_descriptor(html: &str) -> HtmlImageDescriptor {
    if html.len() > MAX_HTML_IMAGE_DESCRIPTOR_BYTES {
        return HtmlImageDescriptor::Rejected;
    }
    let bytes = html.as_bytes();
    let Some(tag_start) = find_ascii_case_insensitive(bytes, b"<img") else {
        return HtmlImageDescriptor::NoImage;
    };
    let tag_end = bytes[tag_start..]
        .iter()
        .position(|byte| *byte == b'>')
        .map_or(bytes.len(), |offset| tag_start + offset);
    let tag = &bytes[tag_start..tag_end];
    let Some(data_start) = find_ascii_case_insensitive(tag, b"data:image/") else {
        return HtmlImageDescriptor::Rejected;
    };
    let kind_start = data_start + b"data:image/".len();
    let Some(kind_end_relative) = tag[kind_start..].iter().position(|byte| *byte == b';') else {
        return HtmlImageDescriptor::Rejected;
    };
    let kind_end = kind_start + kind_end_relative;
    let Some(format) = image_format_from_ascii_hint(&tag[kind_start..kind_end]) else {
        return HtmlImageDescriptor::Rejected;
    };
    let marker = b";base64,";
    if !ascii_slice_eq_ignore_case(tag.get(kind_end..kind_end + marker.len()), marker) {
        return HtmlImageDescriptor::Rejected;
    }
    let encoded_start = kind_end + marker.len();
    let encoded_end = tag[encoded_start..]
        .iter()
        .position(|byte| matches!(*byte, b'\'' | b'"' | b' ' | b'\t' | b'\r' | b'\n'))
        .map_or(tag.len(), |offset| encoded_start + offset);
    let encoded = &tag[encoded_start..encoded_end];
    if encoded.is_empty() || encoded.len() > MAX_DATA_URI_ENCODED_BYTES {
        return HtmlImageDescriptor::Rejected;
    }
    let Ok(encoded) = std::str::from_utf8(encoded) else {
        return HtmlImageDescriptor::Rejected;
    };
    HtmlImageDescriptor::DataUri(EncodedImagePayload::new(format, encoded.to_owned()))
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|candidate| {
        candidate
            .iter()
            .zip(needle)
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    })
}

fn ascii_slice_eq_ignore_case(actual: Option<&[u8]>, expected: &[u8]) -> bool {
    actual.is_some_and(|actual| {
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    })
}

fn image_format_from_ascii_hint(hint: &[u8]) -> Option<ImageFormat> {
    if ascii_slice_eq_ignore_case(Some(hint), b"png") {
        Some(ImageFormat::Png)
    } else if ascii_slice_eq_ignore_case(Some(hint), b"jpeg")
        || ascii_slice_eq_ignore_case(Some(hint), b"jpg")
    {
        Some(ImageFormat::Jpeg)
    } else if ascii_slice_eq_ignore_case(Some(hint), b"gif") {
        Some(ImageFormat::Gif)
    } else if ascii_slice_eq_ignore_case(Some(hint), b"webp") {
        Some(ImageFormat::Webp)
    } else if ascii_slice_eq_ignore_case(Some(hint), b"bmp") {
        Some(ImageFormat::Bmp)
    } else if ascii_slice_eq_ignore_case(Some(hint), b"tiff") {
        Some(ImageFormat::Tiff)
    } else {
        None
    }
}

pub fn image_format_from_path(path: &Path) -> Option<ImageFormat> {
    path.extension().and_then(|extension| {
        match extension.to_string_lossy().to_ascii_lowercase().as_str() {
            "png" => Some(ImageFormat::Png),
            "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
            "gif" => Some(ImageFormat::Gif),
            "webp" => Some(ImageFormat::Webp),
            "svg" => Some(ImageFormat::Svg),
            "bmp" => Some(ImageFormat::Bmp),
            "tif" | "tiff" => Some(ImageFormat::Tiff),
            _ => None,
        }
    })
}

fn image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageMetadata {
    pub resource_id: String,
    pub natural_width: u32,
    pub natural_height: u32,
    pub display_width: Option<u32>,
}

impl ImageMetadata {
    pub fn new(resource_id: impl Into<String>, natural_width: u32, natural_height: u32) -> Self {
        Self {
            resource_id: resource_id.into(),
            natural_width,
            natural_height,
            display_width: None,
        }
    }

    pub fn with_display_width(mut self, width: u32) -> Self {
        self.display_width = Some(width);
        self
    }

    pub fn display_height(&self, available_width: f32) -> f32 {
        image_layout_size(
            available_width,
            (self.natural_width, self.natural_height),
            self.display_width,
        )
        .1
    }

    /// Loading, loaded, and failed nodes deliberately share this geometry.
    pub fn placeholder_height(&self, available_width: f32) -> f32 {
        self.display_height(available_width)
    }
}

/// Shared image geometry for layout estimates, placeholders, hit testing, and
/// the loaded image. The natural aspect ratio is preserved without cropping.
pub(crate) fn image_layout_size(
    available_width: f32,
    natural_size: (u32, u32),
    display_width: Option<u32>,
) -> (f32, f32) {
    let available_width = available_width.max(1.0);
    let natural_width = natural_size.0.max(1) as f32;
    let natural_height = natural_size.1.max(1) as f32;
    let requested_width = display_width.map_or(natural_width, |width| width as f32);
    let width = requested_width.min(available_width).max(1.0);
    let height = (width * natural_height / natural_width).max(1.0);
    (width, height)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageNodeState {
    Loading,
    Loaded,
    Failed,
}

struct StoredImage {
    metadata: ImageMetadata,
    compressed: Option<Vec<u8>>,
    source_path: PathBuf,
    state: ImageNodeState,
    retryable: bool,
}

/// Owns compressed resources and state only.  Decoded CPU/GPU frames belong to
/// the GPUI cache and are dropped there after upload/eviction.
pub struct ImageStore {
    next_id: u64,
    images: HashMap<u64, StoredImage>,
    by_resource_id: HashMap<String, u64>,
    resource_root: PathBuf,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self {
            next_id: 1,
            images: HashMap::new(),
            by_resource_id: HashMap::new(),
            // A store belongs to one retained EditorCore. Never share a
            // process-global cache directory: closing a session must release
            // its materialized sources without deleting another window's
            // visible image files.
            resource_root: std::env::temp_dir()
                .join("joplin-lite-native-images")
                .join(uuid::Uuid::new_v4().simple().to_string()),
        }
    }
}

impl Drop for ImageStore {
    fn drop(&mut self) {
        // `resource_root` is generated per store above, so this can never
        // sweep the shared temp parent. Failure is intentionally best-effort:
        // macOS may still have a decoder handle open briefly.
        let _ = std::fs::remove_dir_all(&self.resource_root);
    }
}

/// `ImageStore` roots live under the process temp directory, so the leaf must
/// be explicitly private even when the host's umask is permissive. The root
/// is generated per retained editor; rejecting a symlink prevents an old or
/// hostile temp entry from redirecting a verified descriptor copy elsewhere.
fn ensure_private_image_materialization_root(resource_root: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(resource_root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "image materialization root is not a private directory",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(resource_root)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    std::fs::set_permissions(resource_root, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Stream bytes into an editor-owned image source with no whole-file buffer
/// and no window where a cache leaf receives the platform default 0644 mode.
fn write_private_image_reader<R: Read>(
    resource_root: &Path,
    destination: &Path,
    reader: &mut R,
) -> std::io::Result<()> {
    ensure_private_image_materialization_root(resource_root)?;
    let filename = destination.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "image materialization destination has no filename",
        )
    })?;
    let temporary = resource_root.join(format!(
        ".{}.{}.tmp",
        filename.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let materialized = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut target = options.open(&temporary)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count])?;
        }
        target.sync_all()?;
        std::fs::rename(&temporary, destination)?;
        #[cfg(unix)]
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    if let Err(error) = materialized {
        // Do not leave an arbitrary partial cache source behind. The durable
        // repository blob remains authoritative and can be materialized again
        // on a later decode attempt.
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

/// Stream a verified durable image into a task-private staging leaf without
/// calling `sync_all`.  Unlike an editor source, this file exists only long
/// enough for ImageIO to make a bounded card proxy; it is never adopted by a
/// document or relied upon after a process crash.  The leaf is still created
/// 0600 with `O_NOFOLLOW`, and failures remove both the staging and destination
/// paths just like the durable materialization path above.
fn write_private_ephemeral_image_reader<R: Read>(
    resource_root: &Path,
    destination: &Path,
    reader: &mut R,
) -> std::io::Result<()> {
    ensure_private_image_materialization_root(resource_root)?;
    let filename = destination.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ephemeral image materialization destination has no filename",
        )
    })?;
    let temporary = resource_root.join(format!(
        ".{}.{}.tmp",
        filename.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let materialized = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut target = options.open(&temporary)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            target.write_all(&buffer[..count])?;
        }
        drop(target);
        std::fs::rename(&temporary, destination)?;
        #[cfg(unix)]
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    if let Err(error) = materialized {
        let _ = std::fs::remove_file(&temporary);
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThumbnailProxyPixelLayout {
    /// CoreGraphics `PremultipliedFirst | Order32Little`: BGRA premultiplied.
    PremultipliedBgra,
    /// resvg/tiny-skia pixmap bytes: RGBA premultiplied.
    PremultipliedRgba,
    /// The pure-Rust raster decoder swaps RGBA to GPUI's straight BGRA.
    StraightBgra,
}

fn thumbnail_proxy_pixel_layout(format: ImageFormat) -> ThumbnailProxyPixelLayout {
    if format == ImageFormat::Svg {
        // tiny-skia::Pixmap::take() is premultiplied RGBA on every target.
        ThumbnailProxyPixelLayout::PremultipliedRgba
    } else if cfg!(target_os = "macos") {
        ThumbnailProxyPixelLayout::PremultipliedBgra
    } else {
        ThumbnailProxyPixelLayout::StraightBgra
    }
}

fn unpremultiply_thumbnail_component(component: u8, alpha: u8) -> u8 {
    debug_assert!(alpha > 0);
    let restored = (u16::from(component) * 255 + u16::from(alpha) / 2) / u16::from(alpha);
    restored.min(255) as u8
}

fn encode_thumbnail_proxy(
    image: &RenderImage,
    max_encoded_bytes: usize,
    pixel_layout: ThumbnailProxyPixelLayout,
) -> std::io::Result<Vec<u8>> {
    let size = image.size(0);
    let width = u32::from(size.width);
    let height = u32::from(size.height);
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| std::io::Error::other("thumbnail proxy dimensions overflow"))?;
    let mut rgba = image
        .as_bytes(0)
        .filter(|bytes| bytes.len() == expected)
        .ok_or_else(|| std::io::Error::other("thumbnail proxy pixels are invalid"))?
        .to_vec();
    // The editor's RenderImage retains its decoder-native layout. PNG needs
    // straight RGBA, so the narrow card-export boundary normalizes each
    // supported layout without changing stable editor pixels.
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3];
        let (red, green, blue, premultiplied) = match pixel_layout {
            ThumbnailProxyPixelLayout::PremultipliedBgra => (pixel[2], pixel[1], pixel[0], true),
            ThumbnailProxyPixelLayout::PremultipliedRgba => (pixel[0], pixel[1], pixel[2], true),
            ThumbnailProxyPixelLayout::StraightBgra => (pixel[2], pixel[1], pixel[0], false),
        };
        let (red, green, blue) = if premultiplied {
            if alpha == 0 {
                // RGB under zero alpha has no visual meaning. Clearing it is
                // deterministic and prevents stale premultiplied color from
                // leaking into the proxy payload.
                (0, 0, 0)
            } else {
                (
                    unpremultiply_thumbnail_component(red, alpha),
                    unpremultiply_thumbnail_component(green, alpha),
                    unpremultiply_thumbnail_component(blue, alpha),
                )
            }
        } else {
            (red, green, blue)
        };
        pixel.copy_from_slice(&[red, green, blue, alpha]);
    }
    let buffer = ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| std::io::Error::other("thumbnail proxy pixels are malformed"))?;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(std::io::Error::other)?;
    let bytes = encoded.into_inner();
    if bytes.len() > max_encoded_bytes {
        return Err(std::io::Error::other(
            "thumbnail proxy exceeds encoded-byte budget",
        ));
    }
    Ok(bytes)
}

impl ImageStore {
    pub fn for_test() -> Self {
        Self::default()
    }

    /// A session-private controlled destination, used only by the retained
    /// resource worker before this store adopts a matching materialization.
    /// It is not a profile/resource-store path and cannot be supplied by UI
    /// input.
    pub(crate) fn materialization_root(&self) -> PathBuf {
        self.resource_root.clone()
    }

    #[cfg(test)]
    pub(crate) fn resource_count_for_test(&self) -> usize {
        self.images.len()
    }

    pub fn insert_invalid_fixture(&mut self, resource_id: impl Into<String>) -> u64 {
        self.insert(
            ImageMetadata::new(resource_id, 1, 1),
            vec![0x00, 0x01, 0x02],
        )
    }

    pub fn insert(&mut self, metadata: ImageMetadata, bytes: Vec<u8>) -> u64 {
        self.insert_inner(metadata, bytes, None)
    }

    pub fn insert_with_format(
        &mut self,
        metadata: ImageMetadata,
        bytes: Vec<u8>,
        format: ImageFormat,
    ) -> u64 {
        self.insert_inner(metadata, bytes, Some(format))
    }

    /// Materialize a durable-resource image without retaining a second copy of
    /// its compressed bytes in the editor entity. Unlike the spike's
    /// best-effort clipboard helper, a write failure returns an error before
    /// this store publishes any resource entry.
    pub fn insert_durable_bytes(
        &mut self,
        metadata: ImageMetadata,
        bytes: &[u8],
        format: ImageFormat,
    ) -> std::io::Result<u64> {
        self.insert_durable_reader(metadata, std::io::Cursor::new(bytes), format)
    }

    /// Stream a descriptor-safe durable blob into this editor's private cache
    /// directory. The temporary file is synced and atomically renamed before
    /// an image entry becomes visible, so a failed materialization has no
    /// half-readable source and no whole-resource allocation.
    pub fn insert_durable_reader<R: Read>(
        &mut self,
        metadata: ImageMetadata,
        reader: R,
        format: ImageFormat,
    ) -> std::io::Result<u64> {
        let source =
            Self::materialize_durable_reader_at(&self.resource_root, &metadata, reader, format)?;
        self.register_materialized_durable_source(metadata, source, format)
    }

    /// Copy a verified reader to one editor-owned path without touching the
    /// live image map.  Task 5 invokes this from the retained background
    /// resource worker; the foreground later performs only the small map
    /// registration after its SQLite snapshot has committed.
    pub(crate) fn materialize_durable_reader_at<R: Read>(
        resource_root: &Path,
        metadata: &ImageMetadata,
        mut reader: R,
        format: ImageFormat,
    ) -> std::io::Result<PathBuf> {
        ensure_private_image_materialization_root(resource_root)?;
        let source = resource_root.join(format!(
            "{}.{}",
            metadata.resource_id,
            image_extension(format)
        ));
        if source.is_file() {
            return Ok(source);
        }
        write_private_image_reader(resource_root, &source, &mut reader)?;
        Ok(source)
    }

    /// Build a small, task-private PNG proxy for an already verified resource
    /// reader.  Card thumbnails cannot retain full originals: a transient
    /// 0600 source exists only while ImageIO downscales it, then is removed;
    /// the final 0600 PNG is the sole cache lease.  The final write is atomic
    /// and synced because it is what the caller may retain across viewports;
    /// the full-sized staging copy is deliberately not synced because it is
    /// never durable state and is discarded before this function returns.
    pub(crate) fn materialize_bounded_thumbnail_proxy_from_verified_reader<R: Read>(
        resource_root: &Path,
        metadata: &ImageMetadata,
        mut reader: R,
        format: ImageFormat,
        max_edge: u32,
        max_encoded_bytes: usize,
    ) -> std::io::Result<PathBuf> {
        ensure_private_image_materialization_root(resource_root)?;
        let staging = resource_root.join(format!(
            ".{}.{}.source.{}",
            metadata.resource_id,
            uuid::Uuid::new_v4().simple(),
            image_extension(format)
        ));
        write_private_ephemeral_image_reader(resource_root, &staging, &mut reader)?;
        let decoded = BudgetedImageCache::decode_resource_bounded_with_max_edge(
            &Resource::from(staging.clone()),
            (max_edge as usize)
                .checked_mul(max_edge as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| std::io::Error::other("thumbnail proxy budget overflow"))?,
            max_edge,
        )
        .map_err(std::io::Error::other);
        let _ = std::fs::remove_file(&staging);
        let decoded = decoded?;
        let encoded = encode_thumbnail_proxy(
            &decoded,
            max_encoded_bytes,
            thumbnail_proxy_pixel_layout(format),
        )?;
        let proxy = resource_root.join(format!("{}.card.png", metadata.resource_id));
        let mut encoded = std::io::Cursor::new(encoded);
        write_private_image_reader(resource_root, &proxy, &mut encoded)?;
        Ok(proxy)
    }

    /// Publish a source that `materialize_durable_reader_at` already copied
    /// into this exact editor's private directory. External paths are never
    /// accepted here: the caller must hand back the deterministic managed
    /// filename for the resource and format.
    pub(crate) fn register_materialized_durable_source(
        &mut self,
        metadata: ImageMetadata,
        source: PathBuf,
        format: ImageFormat,
    ) -> std::io::Result<u64> {
        let expected = self.resource_root.join(format!(
            "{}.{}",
            metadata.resource_id,
            image_extension(format)
        ));
        if source != expected || !source.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "image source is not an editor-managed materialization",
            ));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.by_resource_id.insert(metadata.resource_id.clone(), id);
        self.images.insert(
            id,
            StoredImage {
                metadata,
                compressed: None,
                source_path: source,
                state: ImageNodeState::Loading,
                retryable: false,
            },
        );
        Ok(id)
    }

    /// Register a durable document image whose local bytes cannot currently
    /// be opened. The image atom remains selectable and gets a stable failed
    /// node state, but no compressed blob is retained or fabricated in the
    /// editor process. A later explicit retry/reopen may materialize it from
    /// the repository again.
    pub fn insert_unavailable(&mut self, metadata: ImageMetadata) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let source_path = self
            .resource_root
            .join(format!("{}.unavailable", metadata.resource_id));
        self.by_resource_id.insert(metadata.resource_id.clone(), id);
        self.images.insert(
            id,
            StoredImage {
                metadata,
                compressed: None,
                source_path,
                state: ImageNodeState::Failed,
                retryable: false,
            },
        );
        id
    }

    /// Copy an already materialized image into the managed resource directory
    /// without first reading the source into a Rust `Vec`. This is the
    /// production path for Finder drops and pasteboard temporary files.
    pub fn insert_from_path(
        &mut self,
        metadata: ImageMetadata,
        source_path: &Path,
        format: ImageFormat,
    ) -> std::io::Result<u64> {
        self.insert_durable_reader(metadata, File::open(source_path)?, format)
    }

    fn insert_inner(
        &mut self,
        metadata: ImageMetadata,
        bytes: Vec<u8>,
        format: Option<ImageFormat>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let extension = format.map_or("img", image_extension);
        let source_path = self
            .resource_root
            .join(format!("{}.{}", metadata.resource_id, extension));
        let resource_ready = write_private_image_reader(
            &self.resource_root,
            &source_path,
            &mut std::io::Cursor::new(bytes.as_slice()),
        )
        .is_ok();
        self.by_resource_id.insert(metadata.resource_id.clone(), id);
        self.images.insert(
            id,
            StoredImage {
                metadata,
                // Once the managed resource is durable, the cache can reload
                // it after eviction and the store must not retain a second
                // compressed copy indefinitely. Keep bytes only on a write
                // failure so retry remains possible and observable.
                compressed: (!resource_ready).then_some(bytes),
                source_path,
                state: if resource_ready {
                    ImageNodeState::Loading
                } else {
                    ImageNodeState::Failed
                },
                retryable: !resource_ready,
            },
        );
        id
    }

    /// Retry materializing a resource whose first write failed. Successful
    /// materialization transfers ownership to the managed path and releases
    /// the in-memory compressed copy.
    pub fn retry_resource(&mut self, resource_id: &str) -> bool {
        let Some(id) = self.id_for_resource(resource_id) else {
            return false;
        };
        if !self
            .images
            .get(&id)
            .is_some_and(|image| image.state == ImageNodeState::Failed)
        {
            return false;
        }
        let Some((source_path, bytes)) = self.images.get(&id).and_then(|image| {
            image
                .compressed
                .as_ref()
                .map(|bytes| (image.source_path.clone(), bytes.clone()))
        }) else {
            let ready = self
                .images
                .get(&id)
                .is_some_and(|image| image.source_path.is_file());
            if ready && let Some(image) = self.images.get_mut(&id) {
                image.state = ImageNodeState::Loading;
                image.retryable = false;
            }
            return ready;
        };
        let written = write_private_image_reader(
            source_path.parent().unwrap_or_else(|| Path::new(".")),
            &source_path,
            &mut std::io::Cursor::new(bytes.as_slice()),
        )
        .is_ok();
        if written {
            if let Some(image) = self.images.get_mut(&id) {
                image.compressed = None;
                image.state = ImageNodeState::Loading;
                image.retryable = false;
            }
        }
        written
    }

    pub fn metadata(&self, id: u64) -> Option<&ImageMetadata> {
        self.images.get(&id).map(|image| &image.metadata)
    }

    /// Resolve the stable document resource id used by an image block.  The
    /// numeric handle is an implementation detail and must not be copied
    /// into the document model or renderer.
    pub fn id_for_resource(&self, resource_id: &str) -> Option<u64> {
        self.by_resource_id.get(resource_id).copied()
    }

    /// Roll back a resource that was materialized before its document
    /// transaction failed. The managed file is removed together with the
    /// store entry so failed insertions cannot orphan durable resources.
    pub fn remove_resource(&mut self, resource_id: &str) -> bool {
        let Some(id) = self.by_resource_id.remove(resource_id) else {
            return false;
        };
        let Some(image) = self.images.remove(&id) else {
            return false;
        };
        let _ = std::fs::remove_file(image.source_path);
        true
    }

    pub fn metadata_for_resource(&self, resource_id: &str) -> Option<&ImageMetadata> {
        self.id_for_resource(resource_id)
            .and_then(|id| self.metadata(id))
    }

    pub fn compressed_for_resource(&self, resource_id: &str) -> Option<&[u8]> {
        self.id_for_resource(resource_id)
            .and_then(|id| self.images.get(&id))
            .and_then(|image| image.compressed.as_deref())
    }

    pub fn source_path_for_resource(&self, resource_id: &str) -> Option<&Path> {
        self.id_for_resource(resource_id)
            .and_then(|id| self.images.get(&id))
            .map(|image| image.source_path.as_path())
    }

    pub fn compressed_len(&self, id: u64) -> Option<usize> {
        self.images
            .get(&id)
            .and_then(|image| image.compressed.as_ref())
            .map(Vec::len)
    }

    pub fn finish_loaded(&mut self, id: u64) -> bool {
        let Some(image) = self.images.get_mut(&id) else {
            return false;
        };
        let changed = image.state != ImageNodeState::Loaded || image.retryable;
        image.state = ImageNodeState::Loaded;
        image.retryable = false;
        changed
    }

    pub fn finish_loaded_resource(&mut self, resource_id: &str) -> bool {
        self.id_for_resource(resource_id)
            .is_some_and(|id| self.finish_loaded(id))
    }

    pub fn finish_failed_decode(&mut self, id: u64, _reason: impl Into<String>) -> bool {
        if let Some(image) = self.images.get_mut(&id) {
            let changed = image.state != ImageNodeState::Failed || !image.retryable;
            image.state = ImageNodeState::Failed;
            image.retryable = true;
            return changed;
        }
        false
    }

    pub fn finish_failed_resource(&mut self, resource_id: &str) -> bool {
        if let Some(id) = self.id_for_resource(resource_id) {
            return self.finish_failed_decode(id, "decode failed");
        }
        false
    }

    pub fn node_state(&self, id: u64) -> ImageNodeState {
        self.images
            .get(&id)
            .map_or(ImageNodeState::Failed, |image| image.state)
    }

    pub fn is_selectable(&self, id: u64) -> bool {
        self.images.contains_key(&id)
    }

    pub fn can_retry(&self, id: u64) -> bool {
        self.images.get(&id).is_some_and(|image| image.retryable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDecodeError {
    Overflow,
    InvalidDimensions,
}

pub fn checked_decoded_bytes(frames: &[(u32, u32)]) -> Result<usize, ImageDecodeError> {
    frames.iter().try_fold(0usize, |sum, &(width, height)| {
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ImageDecodeError::Overflow)?;
        sum.checked_add(bytes).ok_or(ImageDecodeError::Overflow)
    })
}

#[derive(Clone, Debug)]
struct TestTexture {
    decoded_bytes: usize,
}

/// Deterministic LRU accounting seam. Production entries use the same budget
/// policy before attaching their real `RenderImage` to `BudgetedImageCache`.
pub struct TextureCache {
    budget_bytes: usize,
    used_bytes: usize,
    entries: HashMap<String, TestTexture>,
    lru: VecDeque<String>,
}

impl TextureCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn insert_for_test(&mut self, key: impl Into<String>, decoded_bytes: usize) {
        let key = key.into();
        self.remove(&key);
        if decoded_bytes > self.budget_bytes {
            return;
        }
        while self.used_bytes.saturating_add(decoded_bytes) > self.budget_bytes {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.remove(&oldest);
        }
        self.used_bytes = self.used_bytes.saturating_add(decoded_bytes);
        self.entries
            .insert(key.clone(), TestTexture { decoded_bytes });
        self.lru.push_back(key);
    }

    fn remove(&mut self, key: &str) {
        if let Some(entry) = self.entries.remove(key) {
            self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
        }
        self.lru.retain(|candidate| candidate != key);
    }
}

struct CachedTexture {
    item: ImageCacheItem,
    decoded_bytes: usize,
    generation: u64,
    proxy_max_edge: u32,
}

struct PendingPromotion {
    generation: u64,
    proxy_max_edge: u32,
}

/// GPUI 0.2.2 image-cache adapter.  It preserves the donor's shared loading
/// task and next-frame notification, while adding exact decoded-byte LRU and
/// `App::drop_image` on eviction.
pub struct BudgetedImageCache {
    budget_bytes: usize,
    used_bytes: usize,
    peak_accounted_bytes: usize,
    entries: HashMap<u64, CachedTexture>,
    lru: VecDeque<u64>,
    visible: HashSet<u64>,
    deferred: HashSet<u64>,
    in_flight: usize,
    reserved_bytes: usize,
    reservations: HashMap<u64, (u64, usize)>,
    pending_retries: HashMap<u64, (u64, Resource)>,
    promotions: HashMap<u64, PendingPromotion>,
    promotion_failures: HashSet<(u64, u32)>,
    promotion_deferred: HashSet<u64>,
    requested_edges: HashMap<u64, u32>,
    harvested_generations: HashSet<(u64, u64)>,
    next_generation: u64,
    #[cfg(test)]
    drop_image_calls: usize,
    weak_entity: Option<WeakEntity<Self>>,
}

impl BudgetedImageCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            peak_accounted_bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            visible: HashSet::new(),
            deferred: HashSet::new(),
            in_flight: 0,
            reserved_bytes: 0,
            reservations: HashMap::new(),
            pending_retries: HashMap::new(),
            promotions: HashMap::new(),
            promotion_failures: HashSet::new(),
            promotion_deferred: HashSet::new(),
            requested_edges: HashMap::new(),
            harvested_generations: HashSet::new(),
            next_generation: 0,
            #[cfg(test)]
            drop_image_calls: 0,
            weak_entity: None,
        }
    }

    /// Create the cache as a GPUI entity and release every retained GPU image
    /// when the entity dies. This mirrors GPUI's donor cache lifecycle instead
    /// of relying on `Arc<RenderImage>` drops to clean the platform atlas.
    pub fn new_entity(cx: &mut App, budget_bytes: usize) -> Entity<Self> {
        let entity = cx.new(|_| Self::new(budget_bytes));
        let weak_entity = entity.downgrade();
        entity.update(cx, |cache, _| cache.weak_entity = Some(weak_entity));
        cx.observe_release(&entity, |cache, cx| {
            for (_, mut entry) in std::mem::take(&mut cache.entries) {
                if let Some(Ok(image)) = entry.item.get() {
                    cache.record_drop_image();
                    cx.drop_image(image, None);
                }
            }
            cache.lru.clear();
            cache.visible.clear();
            cache.deferred.clear();
            cache.in_flight = 0;
            cache.reserved_bytes = 0;
            cache.reservations.clear();
            cache.pending_retries.clear();
            cache.promotions.clear();
            cache.promotion_failures.clear();
            cache.promotion_deferred.clear();
            cache.requested_edges.clear();
            cache.harvested_generations.clear();
            #[cfg(test)]
            {
                cache.drop_image_calls = 0;
            }
            cache.used_bytes = 0;
        })
        .detach();
        entity
    }

    /// Context-local counterpart to [`Self::new_entity`]. LibraryShell is
    /// created inside a retained view context, not directly from `App`; this
    /// preserves the same release hook instead of constructing an untracked
    /// cache that would leak GPU atlas entries when a library window closes.
    pub fn new_entity_in_context<T>(cx: &mut Context<T>, budget_bytes: usize) -> Entity<Self>
    where
        T: 'static,
    {
        cx.new(move |cache_cx| {
            let mut cache = Self::new(budget_bytes);
            cache.weak_entity = Some(cache_cx.weak_entity());
            cache_cx
                .on_release(|cache, app| {
                    for (_, mut entry) in std::mem::take(&mut cache.entries) {
                        if let Some(Ok(image)) = entry.item.get() {
                            cache.record_drop_image();
                            app.drop_image(image, None);
                        }
                    }
                    cache.lru.clear();
                    cache.visible.clear();
                    cache.deferred.clear();
                    cache.in_flight = 0;
                    cache.reserved_bytes = 0;
                    cache.reservations.clear();
                    cache.pending_retries.clear();
                    cache.promotions.clear();
                    cache.promotion_failures.clear();
                    cache.promotion_deferred.clear();
                    cache.requested_edges.clear();
                    cache.harvested_generations.clear();
                    #[cfg(test)]
                    {
                        cache.drop_image_calls = 0;
                    }
                    cache.used_bytes = 0;
                })
                .detach();
            cache
        })
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }
    pub fn peak_accounted_bytes(&self) -> usize {
        self.peak_accounted_bytes
    }
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }
    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes
    }

    fn update_peak_accounted_bytes(&mut self) {
        self.peak_accounted_bytes = self
            .peak_accounted_bytes
            .max(self.used_bytes.saturating_add(self.reserved_bytes));
    }
    pub fn is_settled(&self) -> bool {
        self.in_flight == 0 && self.reserved_bytes == 0
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Read-only presentation state for a managed resource.  List cards use
    /// this after their proxy has entered the cache so a completed ImageIO
    /// failure is not rendered as an indistinguishable loading tile.  It does
    /// not probe the filesystem or schedule a retry; visible-set changes own
    /// the bounded retry policy.
    pub(crate) fn failed_resource(&self, resource: &Resource) -> bool {
        self.entries
            .get(&hash(resource))
            .is_some_and(|entry| matches!(&entry.item, ImageCacheItem::Loaded(Err(_))))
    }

    #[cfg(test)]
    pub(crate) fn loaded_success_for_test(&self, resource: &Resource) -> bool {
        self.entries
            .get(&hash(resource))
            .is_some_and(|entry| matches!(&entry.item, ImageCacheItem::Loaded(Ok(_))))
    }

    #[cfg(test)]
    fn reserved_bytes_for_test(&self) -> usize {
        self.reserved_bytes
    }

    #[cfg(test)]
    fn accounted_bytes_for_test(&self) -> usize {
        self.used_bytes.saturating_add(self.reserved_bytes)
    }

    #[cfg(test)]
    fn drop_image_calls_for_test(&self) -> usize {
        self.drop_image_calls
    }

    fn record_drop_image(&mut self) {
        #[cfg(test)]
        {
            self.drop_image_calls = self.drop_image_calls.saturating_add(1);
        }
    }

    pub fn begin_frame(&mut self) {
        // The complete visible set is installed before sequential loads.
    }

    pub fn mark_visible(&mut self, resource: &Resource) {
        self.visible.insert(hash(resource));
    }

    /// Install the complete visible working set before loading any resource in
    /// that frame. Deferred admissions are retried only after this set changes.
    pub fn set_visible_resources<'a>(&mut self, resources: impl IntoIterator<Item = &'a Resource>) {
        let next = resources.into_iter().map(hash).collect::<HashSet<_>>();
        if next != self.visible {
            self.visible = next;
            self.deferred.clear();
            self.promotion_deferred.clear();
            self.promotion_failures
                .retain(|(key, _)| self.visible.contains(key));
            self.requested_edges
                .retain(|key, _| self.visible.contains(key));
        }
    }

    /// Record the device-pixel requirement for a visible image without source
    /// metadata before its `ImageCache::load` call. A larger request promotes
    /// the proxy; a smaller request does not churn an already-promoted texture.
    pub fn request_edge(&mut self, resource: &Resource, max_edge: u32) {
        self.request_edge_with_natural_max(resource, max_edge, None);
    }

    /// Record a device-pixel requirement together with the source's natural
    /// maximum edge. Quantization may round a viewport request upward, but the
    /// final request must never exceed a valid natural edge or it will create
    /// an impossible promotion after the visible set changes.
    pub fn request_edge_with_natural_max(
        &mut self,
        resource: &Resource,
        max_edge: u32,
        natural_max_edge: Option<u32>,
    ) {
        let key = hash(resource);
        let max_edge = quantize_proxy_edge_with_natural_max(max_edge, natural_max_edge);
        let previous = self.requested_edges.insert(key, max_edge);
        if previous != Some(max_edge) {
            self.deferred.remove(&key);
            self.promotion_deferred.remove(&key);
            self.promotion_failures
                .retain(|(failed_key, _)| *failed_key != key);
        }
        // An edge change is an explicit opportunity to retry a deferred
        // admission/promotion, while repeated paint requests for the same
        // edge remain quiet.
        self.promotion_failures
            .retain(|(failed_key, failed_edge)| *failed_key != key || *failed_edge >= max_edge);
    }

    /// Drop completed entries that have left the current viewport. This uses
    /// the same GPUI image-drop lifecycle as budget eviction, but does not
    /// wait for a later decode admission to create memory pressure.
    pub fn evict_offscreen(&mut self, window: &mut Window, cx: &mut App) {
        let stale = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                (!self.visible.contains(key) && !matches!(entry.item, ImageCacheItem::Loading(_)))
                    .then_some(*key)
            })
            .collect::<Vec<_>>();
        let mut dropped_bytes = 0usize;
        for key in stale {
            let Some(mut entry) = self.entries.remove(&key) else {
                continue;
            };
            self.lru.retain(|candidate| *candidate != key);
            self.requested_edges.remove(&key);
            self.promotion_deferred.remove(&key);
            self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
            dropped_bytes = dropped_bytes.saturating_add(entry.decoded_bytes);
            if let Some(Ok(image)) = entry.item.get() {
                self.record_drop_image();
                cx.drop_image(image, Some(window));
            }
        }
        #[cfg(target_os = "macos")]
        if dropped_bytes >= 4 * 1024 * 1024 {
            mac_pressure::relieve_unused();
        }
    }

    #[cfg(test)]
    fn in_flight_for_test(&self) -> usize {
        self.in_flight
    }

    #[cfg(test)]
    fn deferred_len_for_test(&self) -> usize {
        self.deferred.len()
    }

    /// Remove a failed cache result before retrying the corresponding store
    /// resource. This is deliberately separate from paint so a retry action
    /// cannot leave a permanent cached `Loaded(Err(_))` entry behind.
    pub fn invalidate(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) {
        let key = hash(resource);
        self.deferred.remove(&key);
        self.promotion_deferred.remove(&key);
        self.promotion_failures
            .retain(|(failed_key, _)| *failed_key != key);
        if let Some(entry) = self.entries.remove(&key) {
            if matches!(&entry.item, ImageCacheItem::Loading(_)) {
                // Paint may have already attempted the retry while the shared
                // task still owns the reservation. Keep the target resource
                // until completion releases that slot, then restart it from
                // the same GPUI cache lifecycle.
                self.pending_retries
                    .insert(key, (entry.generation, resource.clone()));
            }
            self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
            if let ImageCacheItem::Loaded(Ok(image)) = &entry.item {
                self.record_drop_image();
                cx.drop_image(image.clone(), Some(window));
            }
        }
        self.lru.retain(|candidate| *candidate != key);
    }

    fn start_promotion(
        &mut self,
        resource: &Resource,
        max_edge: u32,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let key = hash(resource);
        if self.in_flight >= 1
            || self.promotions.contains_key(&key)
            || self.promotion_failures.contains(&(key, max_edge))
            || !self.visible.contains(&key)
        {
            return false;
        }
        let Some(entry) = self.entries.get(&key) else {
            return false;
        };
        if entry.proxy_max_edge >= max_edge || !matches!(entry.item, ImageCacheItem::Loaded(Ok(_)))
        {
            return false;
        }

        let reservation_floor = if self.budget_bytes > CONSERVATIVE_PROXY_RESERVATION {
            CONSERVATIVE_PROXY_RESERVATION
        } else {
            (self.budget_bytes / 2).max(1)
        };
        let floor_fits = self.evict_until_fit(reservation_floor, cx, window);
        let remaining = self
            .budget_bytes
            .saturating_sub(self.used_bytes.saturating_add(self.reserved_bytes));
        if !floor_fits && remaining == 0 {
            return false;
        }
        let reservation = remaining.min(proxy_reservation_bytes(max_edge));
        if reservation < 4 {
            return false;
        }

        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.reserved_bytes = self.reserved_bytes.saturating_add(reservation);
        self.update_peak_accounted_bytes();
        self.reservations.insert(key, (generation, reservation));
        let source = resource.clone();
        let load_future = async move {
            Self::decode_resource_bounded_with_max_edge(&source, reservation, max_edge)
        };
        let task = cx.background_executor().spawn(load_future).shared();
        self.promotions.insert(
            key,
            PendingPromotion {
                generation,
                proxy_max_edge: max_edge,
            },
        );
        self.in_flight = self.in_flight.saturating_add(1);

        let weak_cache = self.weak_entity.clone();
        window
            .spawn(cx, async move |cx| {
                let result = task.await;
                if let Some(cache) = weak_cache {
                    let _ = cache.update_in(cx, |cache, window, entity_cx| {
                        let Some(promotion) = cache.promotions.remove(&key) else {
                            if let Ok(image) = result {
                                cache.record_drop_image();
                                entity_cx.drop_image(image, Some(window));
                            }
                            cache.release_reservation(key, generation);
                            entity_cx.notify();
                            return;
                        };
                        debug_assert_eq!(promotion.generation, generation);

                        let Some(mut entry) = cache.entries.remove(&key) else {
                            if let Ok(image) = result {
                                cache.record_drop_image();
                                entity_cx.drop_image(image, Some(window));
                            }
                            cache.release_reservation(key, generation);
                            entity_cx.notify();
                            return;
                        };
                        cache.lru.retain(|value| *value != key);
                        let old_image = match &entry.item {
                            ImageCacheItem::Loaded(Ok(image)) => Some(image.clone()),
                            _ => None,
                        };
                        let reservation = cache
                            .reservations
                            .get(&key)
                            .filter(|(active_generation, _)| *active_generation == generation)
                            .map(|(_, reservation)| *reservation)
                            .unwrap_or_default();
                        if cache.visible.contains(&key) {
                            if let Ok(image) = result {
                                if let Ok(decoded_bytes) = Self::image_bytes(&image)
                                    && decoded_bytes <= reservation
                                {
                                    let old_bytes = entry.decoded_bytes;
                                    cache.used_bytes = cache
                                        .used_bytes
                                        .saturating_sub(old_bytes)
                                        .saturating_add(decoded_bytes);
                                    let actual_edge = Self::image_max_edge(&image);
                                    entry.decoded_bytes = decoded_bytes;
                                    entry.proxy_max_edge = actual_edge;
                                    if promotion.proxy_max_edge > actual_edge {
                                        cache.promotion_deferred.insert(key);
                                    } else {
                                        cache.promotion_deferred.remove(&key);
                                    }
                                    entry.item = ImageCacheItem::Loaded(Ok(image));
                                    cache.entries.insert(key, entry);
                                    cache.lru.push_back(key);
                                    cache.release_reservation(key, generation);
                                    if let Some(old_image) = old_image {
                                        cache.record_drop_image();
                                        entity_cx.drop_image(old_image, Some(window));
                                    }
                                    entity_cx.notify();
                                    return;
                                } else {
                                    cache.record_drop_image();
                                    entity_cx.drop_image(image, Some(window));
                                }
                            }
                        } else if let Ok(image) = result {
                            cache.record_drop_image();
                            entity_cx.drop_image(image, Some(window));
                        }

                        cache.entries.insert(key, entry);
                        cache.lru.push_back(key);
                        cache
                            .promotion_failures
                            .insert((key, promotion.proxy_max_edge));
                        cache.release_reservation(key, generation);
                        entity_cx.notify();
                    });
                }
            })
            .detach();
        true
    }

    fn image_bytes(image: &RenderImage) -> Result<usize, ImageDecodeError> {
        let frames = (0..image.frame_count())
            .map(|index| {
                let size = image.size(index);
                (u32::from(size.width), u32::from(size.height))
            })
            .collect::<Vec<_>>();
        checked_decoded_bytes(&frames)
    }

    fn image_max_edge(image: &RenderImage) -> u32 {
        (0..image.frame_count())
            .map(|index| {
                let size = image.size(index);
                u32::from(size.width).max(u32::from(size.height))
            })
            .max()
            .unwrap_or(1)
    }

    fn bounded_svg_dimensions(
        source_width: f32,
        source_height: f32,
        budget_bytes: usize,
        max_edge: u32,
    ) -> anyhow::Result<(u32, u32)> {
        if !source_width.is_finite()
            || !source_height.is_finite()
            || source_width <= 0.0
            || source_height <= 0.0
            || max_edge == 0
        {
            return Err(anyhow!("SVG has invalid dimensions"));
        }
        let max_pixels = budget_bytes / 4;
        if max_pixels == 0 {
            return Err(anyhow!("decoded image budget is too small"));
        }
        let source_width = f64::from(source_width);
        let source_height = f64::from(source_height);
        let source_width_px = source_width.ceil() as u64;
        let source_height_px = source_height.ceil() as u64;
        let source_pixels = source_width_px
            .checked_mul(source_height_px)
            .ok_or_else(|| anyhow!("SVG dimensions overflow"))?;
        if source_pixels == 0 {
            return Err(anyhow!("SVG dimensions overflow"));
        }
        let edge_scale = (f64::from(max_edge) / source_width.max(source_height)).min(1.0);
        let budget_scale = ((max_pixels as f64) / source_pixels as f64).sqrt().min(1.0);
        let scale = edge_scale.min(budget_scale);
        let mut width = (source_width * scale).floor().max(1.0) as u32;
        let mut height = (source_height * scale).floor().max(1.0) as u32;
        while (width as usize)
            .checked_mul(height as usize)
            .map_or(true, |pixels| pixels > max_pixels)
        {
            if width >= height && width > 1 {
                width -= 1;
            } else if height > 1 {
                height -= 1;
            } else {
                return Err(anyhow!("SVG dimensions cannot fit decoded budget"));
            }
        }
        Ok((width, height))
    }

    /// Adapted from GPUI 0.2.2 `Image::to_image_data`: decode each format,
    /// resize while still in image buffers, convert RGBA to GPUI BGRA, and
    /// construct exactly one bounded `RenderImage`.
    fn decode_bounded(
        bytes: &[u8],
        budget_bytes: usize,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        Self::decode_bounded_with_max_edge(bytes, budget_bytes, DEFAULT_PROXY_MAX_EDGE)
    }

    fn decode_bounded_with_max_edge(
        bytes: &[u8],
        budget_bytes: usize,
        max_edge: u32,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        let guessed_format = image::guess_format(bytes);
        let is_svg = guessed_format.is_err();
        let mut frames = if let Ok(format) = guessed_format {
            if format == image::ImageFormat::Gif {
                // Animated GIF playback is intentionally deferred for this
                // spike. Retain one stable first frame instead of decoding
                // and holding every unused frame in the editor cache.
                let frame = GifDecoder::new(std::io::Cursor::new(bytes))?
                    .into_frames()
                    .next()
                    .ok_or_else(|| anyhow!("GIF contains no frames"))??;
                vec![frame]
            } else {
                vec![image::Frame::new(
                    image::load_from_memory_with_format(bytes, format)?.into_rgba8(),
                )]
            }
        } else {
            let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
                .map_err(|error| anyhow!(error.to_string()))?;
            let svg_size = tree.size();
            let (target_width, target_height) = Self::bounded_svg_dimensions(
                svg_size.width(),
                svg_size.height(),
                budget_bytes,
                max_edge,
            )?;
            let mut pixmap = resvg::tiny_skia::Pixmap::new(target_width, target_height)
                .ok_or_else(|| anyhow!("SVG renderer returned invalid size"))?;
            let transform = resvg::tiny_skia::Transform::from_scale(
                (target_width as f32 / svg_size.width())
                    .min(target_height as f32 / svg_size.height()),
                (target_width as f32 / svg_size.width())
                    .min(target_height as f32 / svg_size.height()),
            );
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            vec![image::Frame::new(
                ImageBuffer::from_raw(pixmap.width(), pixmap.height(), pixmap.take())
                    .ok_or_else(|| anyhow!("SVG renderer returned invalid pixels"))?,
            )]
        };
        let decoded_bytes =
            frames
                .iter()
                .try_fold(0usize, |sum, frame| -> anyhow::Result<usize> {
                    let (width, height) = frame.buffer().dimensions();
                    sum.checked_add(
                        (width as usize)
                            .checked_mul(height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                            .ok_or_else(|| anyhow!("decoded image dimensions overflow"))?,
                    )
                    .ok_or_else(|| anyhow!("decoded image budget overflow"))
                })?;
        let max_pixels = budget_bytes / 4;
        if max_pixels < frames.len() {
            return Err(ImageCacheError::from(anyhow!(
                "decoded animation exceeds minimum budget"
            )));
        }
        let source_max_edge = frames
            .iter()
            .map(|frame| {
                let (width, height) = frame.buffer().dimensions();
                width.max(height)
            })
            .max()
            .unwrap_or(1);
        let edge_scale = (f64::from(max_edge.max(1)) / f64::from(source_max_edge)).min(1.0);
        let budget_scale = ((max_pixels as f64 * frames.len() as f64) / (decoded_bytes / 4) as f64)
            .sqrt()
            .min(1.0);
        let scale = edge_scale.min(budget_scale);
        if scale < 1.0 {
            frames = frames
                .into_iter()
                .map(|frame| {
                    let (width, height) = frame.buffer().dimensions();
                    let target_width = ((width as f64 * scale).floor() as u32).max(1);
                    let target_height = ((height as f64 * scale).floor() as u32).max(1);
                    let resized = image::imageops::resize(
                        frame.buffer(),
                        target_width,
                        target_height,
                        FilterType::Triangle,
                    );
                    image::Frame::from_parts(resized, 0, 0, frame.delay())
                })
                .collect();
        }
        let bounded_bytes: anyhow::Result<usize> =
            frames
                .iter()
                .try_fold(0usize, |sum, frame| -> anyhow::Result<usize> {
                    let (width, height) = frame.buffer().dimensions();
                    sum.checked_add(
                        (width as usize)
                            .checked_mul(height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                            .ok_or_else(|| anyhow!("decoded image dimensions overflow"))?,
                    )
                    .ok_or_else(|| anyhow!("decoded image budget overflow"))
                });
        let bounded_bytes = bounded_bytes?;
        if bounded_bytes > budget_bytes {
            return Err(ImageCacheError::from(anyhow!(
                "decoded image remains over the hard budget after resize"
            )));
        }
        // GPUI's ordinary bitmap path expects BGRA (the same conversion used
        // by platform.rs::Image::to_image_data). resvg's tiny-skia pixmap is
        // premultiplied RGBA, and the existing RenderImage path retains that
        // native pixmap layout; do not apply the raster B/R swap here. The
        // separate card-PNG exporter explicitly handles this layout.
        if !is_svg {
            for frame in &mut frames {
                for pixel in frame.buffer_mut().chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }
        }
        let frames = frames.into_iter().collect::<SmallVec<[image::Frame; 1]>>();
        Ok(Arc::new(RenderImage::new(frames)))
    }

    pub(crate) fn decode_resource_bounded(
        resource: &Resource,
        budget_bytes: usize,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        Self::decode_resource_bounded_with_max_edge(resource, budget_bytes, DEFAULT_PROXY_MAX_EDGE)
    }

    fn decode_resource_bounded_with_max_edge(
        resource: &Resource,
        budget_bytes: usize,
        max_edge: u32,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        let result = match resource {
            Resource::Path(path) => {
                #[cfg(target_os = "macos")]
                {
                    if path.extension().is_some_and(|extension| {
                        extension.to_string_lossy().eq_ignore_ascii_case("svg")
                    }) {
                        let bytes = std::fs::read(path.as_ref()).map_err(ImageCacheError::from)?;
                        Self::decode_bounded_with_max_edge(&bytes, budget_bytes, max_edge)
                    } else {
                        Self::decode_macos_path(path, budget_bytes, max_edge)
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let bytes = std::fs::read(path.as_ref()).map_err(ImageCacheError::from)?;
                    Self::decode_bounded_with_max_edge(&bytes, budget_bytes, max_edge)
                }
            }
            _ => Err(ImageCacheError::from(anyhow!(
                "native images require a managed path resource"
            ))),
        };
        #[cfg(target_os = "macos")]
        if result.is_ok()
            && let Resource::Path(path) = resource
        {
            let decoded_bytes = result
                .as_ref()
                .ok()
                .and_then(|image| Self::image_bytes(image).ok())
                .unwrap_or_default();
            mac_pressure::relieve_for_path(path, decoded_bytes);
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn decode_macos_path(
        path: &Path,
        budget_bytes: usize,
        max_edge: u32,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType, CFURL};
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        use objc2_core_graphics::{
            CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
            CGImageByteOrderInfo,
        };
        use objc2_image_io::{
            CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceShouldCache,
            kCGImageSourceThumbnailMaxPixelSize,
        };

        let url = CFURL::from_file_path(path).ok_or_else(|| {
            ImageCacheError::from(anyhow!("managed image path cannot become a file URL"))
        })?;
        let source = unsafe { CGImageSource::with_url(&url, None) }.ok_or_else(|| {
            ImageCacheError::from(anyhow!("ImageIO could not open managed image resource"))
        })?;
        // The MVP renderer paints frame zero and has no animation scheduler;
        // retain one ImageIO frame for every multi-frame source, not only GIF.
        let source_frame_count = unsafe { source.count() }.max(1);
        let frame_count = 1usize;
        let max_pixels_per_frame = budget_bytes
            .checked_div(frame_count)
            .and_then(|bytes| bytes.checked_div(4))
            .ok_or_else(|| ImageCacheError::from(anyhow!("decoded image budget is too small")))?;
        let budget_edge = (max_pixels_per_frame as f64).sqrt().floor() as u32;
        let max_edge = max_edge.min(budget_edge.max(1));
        let create_thumbnail = CFBoolean::new(true);
        let transform = CFBoolean::new(true);
        let should_cache = CFBoolean::new(false);
        let max_pixel_size = CFNumber::new_i32(max_edge as i32);
        let keys: [&CFString; 4] = unsafe {
            [
                kCGImageSourceCreateThumbnailFromImageAlways,
                kCGImageSourceCreateThumbnailWithTransform,
                kCGImageSourceShouldCache,
                kCGImageSourceThumbnailMaxPixelSize,
            ]
        };
        let values: [&CFType; 4] = [
            create_thumbnail.as_ref(),
            transform.as_ref(),
            should_cache.as_ref(),
            max_pixel_size.as_ref(),
        ];
        let options =
            CFDictionary::<CFType, CFType>::from_slices(&keys.map(|key| key as &CFType), &values);

        let mut frames = SmallVec::<[image::Frame; 1]>::new();
        for index in 0..frame_count.min(source_frame_count) {
            let image = unsafe { source.thumbnail_at_index(index, Some(options.as_ref())) }
                .ok_or_else(|| {
                    ImageCacheError::from(anyhow!("ImageIO could not create bounded thumbnail"))
                })?;
            let width = CGImage::width(Some(&image));
            let height = CGImage::height(Some(&image));
            if width == 0 || height == 0 {
                return Err(ImageCacheError::from(anyhow!(
                    "ImageIO returned an empty thumbnail bitmap"
                )));
            }
            let output_len = width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| ImageCacheError::from(anyhow!("thumbnail dimensions overflow")))?;
            let mut pixels = vec![0u8; output_len];
            let color_space = CGColorSpace::new_device_rgb().ok_or_else(|| {
                ImageCacheError::from(anyhow!("CoreGraphics could not create RGB color space"))
            })?;
            let bytes_per_row = width
                .checked_mul(4)
                .ok_or_else(|| ImageCacheError::from(anyhow!("thumbnail row overflows")))?;
            let bitmap_info =
                CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
            {
                let context = unsafe {
                    CGBitmapContextCreate(
                        pixels.as_mut_ptr().cast(),
                        width,
                        height,
                        8,
                        bytes_per_row,
                        Some(&color_space),
                        bitmap_info,
                    )
                }
                .ok_or_else(|| {
                    ImageCacheError::from(anyhow!("CoreGraphics could not create bitmap context"))
                })?;
                CGContext::draw_image(
                    Some(&context),
                    CGRect::new(
                        CGPoint::new(0.0, 0.0),
                        CGSize::new(width as f64, height as f64),
                    ),
                    Some(&image),
                );
            }
            let buffer = ImageBuffer::from_raw(width as u32, height as u32, pixels)
                .ok_or_else(|| ImageCacheError::from(anyhow!("thumbnail pixels invalid")))?;
            frames.push(image::Frame::new(buffer));
            drop(image);
            // The thumbnail is the only decoded representation retained by
            // this cache. Explicitly remove ImageIO's source-side cache as
            // well, so a large managed original cannot stay resident after
            // the bounded proxy has been copied into the GPUI frame.
            unsafe { source.remove_cache_at_index(index) };
        }

        let decoded_bytes = frames
            .iter()
            .map(|frame| {
                let (width, height) = frame.buffer().dimensions();
                (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4)
            })
            .sum::<usize>();
        if decoded_bytes > budget_bytes {
            return Err(ImageCacheError::from(anyhow!(
                "ImageIO bounded proxy exceeds decoded-image budget"
            )));
        }
        Ok(Arc::new(RenderImage::new(frames)))
    }

    fn evict_until_fit(&mut self, needed: usize, cx: &mut App, window: &mut Window) -> bool {
        while self
            .used_bytes
            .saturating_add(self.reserved_bytes)
            .saturating_add(needed)
            > self.budget_bytes
        {
            let Some(index) = self.lru.iter().position(|key| {
                !self.visible.contains(key)
                    && self
                        .entries
                        .get(key)
                        .is_some_and(|entry| !matches!(entry.item, ImageCacheItem::Loading(_)))
            }) else {
                return false;
            };
            let Some(oldest) = self.lru.remove(index) else {
                return false;
            };
            if let Some(mut entry) = self.entries.remove(&oldest) {
                self.promotion_deferred.remove(&oldest);
                self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                if let Some(Ok(image)) = entry.item.get() {
                    self.record_drop_image();
                    cx.drop_image(image, Some(window));
                }
            }
        }
        true
    }

    fn release_reservation(&mut self, key: u64, generation: u64) -> usize {
        let Some((reservation_generation, reservation)) = self.reservations.get(&key).copied()
        else {
            return 0;
        };
        if reservation_generation != generation {
            return 0;
        }
        self.reservations.remove(&key);
        self.reserved_bytes = self.reserved_bytes.saturating_sub(reservation);
        self.in_flight = self.in_flight.saturating_sub(1);
        reservation
    }

    fn completion_is_current(&self, key: u64, generation: u64) -> bool {
        self.reservations
            .get(&key)
            .is_some_and(|(active_generation, _)| *active_generation == generation)
            || self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
    }

    fn finalize_completion(&mut self, key: u64, generation: u64) -> CompletionState {
        if !self.completion_is_current(key, generation) {
            return CompletionState::Stale {
                harvested: self.harvested_generations.remove(&(key, generation)),
            };
        }
        self.release_reservation(key, generation);
        let Some(entry) = self.entries.remove(&key) else {
            let pending_retry = self
                .pending_retries
                .get(&key)
                .filter(|(pending_generation, _)| *pending_generation == generation)
                .map(|(_, resource)| resource.clone());
            if pending_retry.is_some() {
                self.pending_retries.remove(&key);
            }
            return CompletionState::Missing { pending_retry };
        };
        self.lru.retain(|value| *value != key);
        if matches!(&entry.item, ImageCacheItem::Loading(_)) {
            CompletionState::Loading(entry)
        } else {
            self.harvested_generations.remove(&(key, generation));
            CompletionState::Harvested(entry)
        }
    }
}

enum CompletionState {
    Stale { harvested: bool },
    Missing { pending_retry: Option<Resource> },
    Harvested(CachedTexture),
    Loading(CachedTexture),
}

impl ImageCache for BudgetedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        let max_edge = self.requested_edges.get(&key).copied().unwrap_or_else(|| {
            quantize_proxy_edge(proxy_max_edge_for_viewport(
                f32::from(window.viewport_size().width),
                window.scale_factor(),
            ))
        });
        if self.promotions.contains_key(&key) {
            // A promotion is stale-while-revalidate: keep returning the
            // settled proxy while the higher-resolution task is in flight.
            return self
                .entries
                .get_mut(&key)
                .and_then(|entry| entry.item.get());
        }
        if self.promotion_deferred.contains(&key) {
            // The last decode was bounded by available capacity. Keep the
            // actual proxy drawable without retrying it on every paint; a
            // visible-set/capacity change clears this gate.
            return self
                .entries
                .get_mut(&key)
                .and_then(|entry| entry.item.get());
        }
        let should_promote = self.entries.get(&key).is_some_and(|entry| {
            max_edge > entry.proxy_max_edge && matches!(entry.item, ImageCacheItem::Loaded(Ok(_)))
        });
        if should_promote {
            self.start_promotion(resource, max_edge, window, cx);
            return self
                .entries
                .get_mut(&key)
                .and_then(|entry| entry.item.get());
        }
        if let Some(mut entry) = self.entries.remove(&key) {
            self.lru.retain(|value| *value != key);
            let was_loading = matches!(&entry.item, ImageCacheItem::Loading(_));
            let result = entry.item.get();
            if was_loading && result.is_some() {
                self.release_reservation(key, entry.generation);
                self.harvested_generations.insert((key, entry.generation));
            }
            if let Some(Ok(ref image)) = result {
                let bytes = match Self::image_bytes(image) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        let error =
                            ImageCacheError::from(anyhow!("decoded image dimensions overflow"));
                        entry.item = ImageCacheItem::Loaded(Err(error.clone()));
                        self.entries.insert(key, entry);
                        self.lru.push_back(key);
                        return Some(Err(error));
                    }
                };
                if bytes > self.budget_bytes {
                    let error = ImageCacheError::from(anyhow!(
                        "cached image exceeds the hard decoded-image budget"
                    ));
                    self.record_drop_image();
                    cx.drop_image(image.clone(), Some(window));
                    self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                    entry.decoded_bytes = 0;
                    entry.item = ImageCacheItem::Loaded(Err(error.clone()));
                    self.entries.insert(key, entry);
                    self.lru.push_back(key);
                    return Some(Err(error));
                }
                let actual_edge = Self::image_max_edge(image);
                entry.proxy_max_edge = actual_edge;
                if max_edge > actual_edge {
                    self.promotion_deferred.insert(key);
                } else {
                    self.promotion_deferred.remove(&key);
                }
                self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                if !self.evict_until_fit(bytes, cx, window) {
                    self.record_drop_image();
                    cx.drop_image(image.clone(), Some(window));
                    // Capacity pressure is a deferred admission, not a
                    // decode failure. Drop the proxy and retry only after a
                    // visible-set or capacity change.
                    self.promotion_deferred.remove(&key);
                    self.deferred.insert(key);
                    return None;
                }
                self.used_bytes = self.used_bytes.saturating_add(bytes);
                self.update_peak_accounted_bytes();
                entry.decoded_bytes = bytes;
            }
            if let Some(result) = result.clone() {
                entry.item = ImageCacheItem::Loaded(result);
            }
            self.entries.insert(key, entry);
            self.lru.push_back(key);
            return result;
        }

        if self.deferred.contains(&key) {
            return None;
        }

        if self.in_flight >= 1 {
            return None;
        }
        // Keep a meaningful proxy allowance for production-sized caches, but
        // let tiny lifecycle fixtures reserve their remaining half-budget.
        // The eviction pass still uses this as a floor before admission.
        let reservation_floor = if self.budget_bytes > CONSERVATIVE_PROXY_RESERVATION {
            CONSERVATIVE_PROXY_RESERVATION
        } else {
            (self.budget_bytes / 2).max(1)
        };
        let floor_fits = self.evict_until_fit(reservation_floor, cx, window);
        let remaining = self
            .budget_bytes
            .saturating_sub(self.used_bytes.saturating_add(self.reserved_bytes));
        if !floor_fits && remaining == 0 {
            self.deferred.insert(key);
            return None;
        }
        // A fixed proxy floor is only a scheduling hint. If visible retained
        // images leave less than that floor, reserve the exact remaining
        // bytes instead of permanently deferring a proxy that can fit.
        let reservation = remaining.min(proxy_reservation_bytes(max_edge));
        // A RenderImage must contain at least one complete RGBA pixel. A
        // smaller remainder is still capacity pressure, not a decoder error;
        // leave it deferred until the visible set/capacity changes.
        if reservation < 4 {
            self.deferred.insert(key);
            return None;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.reserved_bytes = self.reserved_bytes.saturating_add(reservation);
        self.update_peak_accounted_bytes();
        self.reservations.insert(key, (generation, reservation));
        let budget = reservation;
        let source = resource.clone();
        let load_future =
            async move { Self::decode_resource_bounded_with_max_edge(&source, budget, max_edge) };
        let task = cx.background_executor().spawn(load_future).shared();
        self.entries.insert(
            key,
            CachedTexture {
                item: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
                generation,
                proxy_max_edge: max_edge,
            },
        );
        self.in_flight = self.in_flight.saturating_add(1);
        self.lru.push_back(key);
        let weak_cache = self.weak_entity.clone();
        window
            .spawn(cx, async move |cx| {
                let result = task.await;
                if let Some(cache) = weak_cache {
                    let _ = cache.update_in(cx, |cache, window, entity_cx| {
                        let mut entry = match cache.finalize_completion(key, generation) {
                            CompletionState::Stale { harvested } => {
                                if !harvested && let Ok(image) = result.clone() {
                                    cache.record_drop_image();
                                    entity_cx.drop_image(image, Some(window));
                                }
                                return;
                            }
                            CompletionState::Missing { pending_retry } => {
                                if let Ok(image) = result.clone() {
                                    cache.record_drop_image();
                                    entity_cx.drop_image(image, Some(window));
                                }
                                if let Some(retry_resource) = pending_retry
                                    && cache.visible.contains(&key)
                                {
                                    // The retry was requested while the old
                                    // shared task still occupied the only
                                    // scheduler slot. Restart it now, from the
                                    // same ImageCache lifecycle, and notify the
                                    // editor even if the entry was invalidated.
                                    let _ = cache.load(&retry_resource, window, entity_cx);
                                }
                                entity_cx.notify();
                                return;
                            }
                            CompletionState::Harvested(entry) => {
                                cache.entries.insert(key, entry);
                                cache.lru.push_back(key);
                                return;
                            }
                            CompletionState::Loading(entry) => entry,
                        };

                        match result {
                            Ok(image) => {
                                let decoded_bytes = match Self::image_bytes(&image) {
                                    Ok(bytes) => bytes,
                                    Err(_) => {
                                        cache.record_drop_image();
                                        entity_cx.drop_image(image, Some(window));
                                        entity_cx.notify();
                                        return;
                                    }
                                };
                                if !cache.visible.contains(&key)
                                    || decoded_bytes > cache.budget_bytes
                                    || !cache.evict_until_fit(decoded_bytes, entity_cx, window)
                                {
                                    cache.record_drop_image();
                                    entity_cx.drop_image(image, Some(window));
                                    if cache.visible.contains(&key) {
                                        cache.deferred.insert(key);
                                    }
                                    entity_cx.notify();
                                    return;
                                }
                                let requested_edge = cache
                                    .requested_edges
                                    .get(&key)
                                    .copied()
                                    .unwrap_or(entry.proxy_max_edge);
                                let actual_edge = Self::image_max_edge(&image);
                                entry.proxy_max_edge = actual_edge;
                                if requested_edge > actual_edge {
                                    cache.promotion_deferred.insert(key);
                                } else {
                                    cache.promotion_deferred.remove(&key);
                                }
                                cache.used_bytes = cache.used_bytes.saturating_add(decoded_bytes);
                                cache.update_peak_accounted_bytes();
                                entry.decoded_bytes = decoded_bytes;
                                entry.item = ImageCacheItem::Loaded(Ok(image));
                                cache.entries.insert(key, entry);
                                cache.lru.push_back(key);
                            }
                            Err(error) => {
                                if cache.visible.contains(&key) {
                                    entry.item = ImageCacheItem::Loaded(Err(error));
                                    cache.entries.insert(key, entry);
                                    cache.lru.push_back(key);
                                }
                            }
                        }
                        entity_cx.notify();
                    });
                }
            })
            .detach();
        None
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, Default)]
struct NativePasteboardSnapshot {
    images: Vec<ImagePayload>,
    html: Option<String>,
    rich_text: Option<String>,
    text: Option<String>,
    file_urls: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
fn native_payload_from_snapshot(snapshot: NativePasteboardSnapshot) -> Option<ClipboardPayload> {
    let NativePasteboardSnapshot {
        images,
        html,
        rich_text,
        text,
        file_urls,
    } = snapshot;
    if !images.is_empty() {
        return Some(ClipboardPayload {
            images: bounded_image_candidates(images),
            ..Default::default()
        });
    }
    if !file_urls.is_empty() {
        return Some(ClipboardPayload {
            file_urls,
            html,
            rich_text,
            text,
            ..Default::default()
        });
    }
    if html.is_some() || rich_text.is_some() || text.is_some() {
        Some(ClipboardPayload {
            html,
            rich_text,
            text,
            ..Default::default()
        })
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
const NATIVE_IMAGE_UTIS: &[(ImageFormat, &str)] = &[
    (ImageFormat::Png, "public.png"),
    (ImageFormat::Tiff, "public.tiff"),
    (ImageFormat::Jpeg, "public.jpeg"),
    (ImageFormat::Gif, "com.compuserve.gif"),
    (ImageFormat::Webp, "org.webmproject.webp"),
    (ImageFormat::Bmp, "com.microsoft.bmp"),
    (ImageFormat::Svg, "public.svg-image"),
];

/// One macOS paste can expose the same visual content through several UTI
/// encodings.  Probe every supported UTI in order, but keep no more than a
/// 30 MiB process-owned compressed-byte budget.  A too-large earlier UTI does
/// not stop a later, smaller valid representation from being considered.
#[cfg(target_os = "macos")]
const MAX_NATIVE_IMAGE_CANDIDATE_BYTES: usize = 3 * app_lite_core::MAX_IMAGE_BYTES;

/// The AppKit bridge returns only process-owned payloads. This seam keeps the
/// UTI preference independently testable without creating a pasteboard or
/// permitting foreground storage access.
#[cfg(target_os = "macos")]
enum NativeImageRead {
    Missing,
    Rejected,
    Payloads(Vec<ImagePayload>),
}

#[cfg(target_os = "macos")]
fn native_image_candidates_from(
    mut payload_for_uti: impl FnMut(ImageFormat, &str, usize) -> NativeImageRead,
) -> NativeImageRead {
    let mut any_rejected = false;
    let mut candidates = Vec::new();
    let mut total_bytes = 0usize;
    for &(format, uti) in NATIVE_IMAGE_UTIS {
        let remaining_budget = MAX_NATIVE_IMAGE_CANDIDATE_BYTES.saturating_sub(total_bytes);
        match payload_for_uti(format, uti, remaining_budget) {
            NativeImageRead::Missing => continue,
            NativeImageRead::Rejected => any_rejected = true,
            NativeImageRead::Payloads(payloads) => {
                for payload in payloads {
                    let Some(next_total) = total_bytes.checked_add(payload.bytes.len()) else {
                        any_rejected = true;
                        break;
                    };
                    if next_total > MAX_NATIVE_IMAGE_CANDIDATE_BYTES {
                        any_rejected = true;
                        break;
                    }
                    total_bytes = next_total;
                    candidates.push(payload);
                }
            }
        }
    }
    if !candidates.is_empty() {
        NativeImageRead::Payloads(candidates)
    } else if any_rejected {
        NativeImageRead::Rejected
    } else {
        NativeImageRead::Missing
    }
}

/// Copy a pasted image while AppKit owns the source bytes. A `Vec` is an
/// explicit process-owned handoff to the existing background staging path;
/// it is neither a temporary file nor a borrowed Foundation buffer.
#[cfg(target_os = "macos")]
fn copy_bounded_native_image_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > app_lite_core::MAX_IMAGE_BYTES {
        return None;
    }
    Some(bytes.to_vec())
}

#[cfg(target_os = "macos")]
unsafe fn native_string_value(value: cocoa::base::id) -> Option<String> {
    use cocoa::base::nil;
    use cocoa::foundation::NSString;
    if value == nil {
        return None;
    }
    let bytes = unsafe { value.UTF8String() as *const u8 };
    if bytes.is_null() {
        return Some(String::new());
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(bytes as *const std::ffi::c_char).to_bytes() };
    std::str::from_utf8(bytes).map(str::to_owned).ok()
}

#[cfg(target_os = "macos")]
unsafe fn native_rtf_string_value(data: cocoa::base::id) -> Option<String> {
    use cocoa::base::nil;
    use objc::{class, msg_send, sel, sel_impl};
    if data == nil {
        return None;
    }
    let allocated: cocoa::base::id = msg_send![class!(NSAttributedString), alloc];
    if allocated == nil {
        return None;
    }
    let nil_id: cocoa::base::id = nil;
    let attributed: cocoa::base::id = msg_send![
        allocated,
        initWithRTF: data
        documentAttributes: nil_id
    ];
    if attributed == nil {
        // `initWithRTF:documentAttributes:` consumes the alloc/init receiver
        // even when Foundation rejects malformed bytes. Releasing it again
        // here is a double-release and can abort the paste action under MRC.
        return None;
    }
    let string: cocoa::base::id = msg_send![attributed, string];
    let result = unsafe { native_string_value(string) };
    let _: () = msg_send![attributed, release];
    result
}

#[cfg(target_os = "macos")]
unsafe fn native_image_payload(
    data: cocoa::base::id,
    format: ImageFormat,
    remaining_budget: usize,
) -> NativeImageRead {
    use cocoa::base::nil;
    use objc::{msg_send, sel, sel_impl};

    if data == nil {
        return NativeImageRead::Missing;
    }
    let length: usize = unsafe { msg_send![data, length] };
    if length == 0 || length > app_lite_core::MAX_IMAGE_BYTES || length > remaining_budget {
        return NativeImageRead::Rejected;
    }
    let source: *const u8 = unsafe { msg_send![data, bytes] };
    if source.is_null() {
        return NativeImageRead::Rejected;
    }
    let source = unsafe { std::slice::from_raw_parts(source, length) };
    copy_bounded_native_image_bytes(source)
        .map(|bytes| NativeImageRead::Payloads(vec![ImagePayload::new(format, bytes)]))
        .unwrap_or(NativeImageRead::Rejected)
}

#[cfg(target_os = "macos")]
pub fn read_native_pasteboard() -> Option<ClipboardPayload> {
    // Narrow AppKit bridge: only pasteboard extraction happens here. The
    // editor model, layout, and rendering remain GPUI/native-editor owned.
    use cocoa::appkit::{NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeString};
    use cocoa::base::nil;
    use cocoa::foundation::{NSArray, NSAutoreleasePool, NSString};

    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        match native_image_candidates_from(|format, uti, remaining_budget| {
            let ty = NSString::alloc(nil).init_str(uti).autorelease();
            native_image_payload(pasteboard.dataForType(ty), format, remaining_budget)
        }) {
            NativeImageRead::Payloads(images) => {
                return native_payload_from_snapshot(NativePasteboardSnapshot {
                    images,
                    ..Default::default()
                });
            }
            // A rejected image must not fall through to its accompanying
            // screenshot placeholder text. The user can retry with a smaller
            // image, while the foreground path remains bounded and disk-free.
            NativeImageRead::Rejected => {
                return Some(ClipboardPayload {
                    native_image_rejected: true,
                    ..ClipboardPayload::default()
                });
            }
            NativeImageRead::Missing => {}
        }
        let files = pasteboard.propertyListForType(NSFilenamesPboardType);
        if files != nil {
            let mut snapshot = NativePasteboardSnapshot::default();
            for index in 0..files.count() {
                if let Some(path) = native_string_value(files.objectAtIndex(index)) {
                    snapshot.file_urls.push(PathBuf::from(path));
                }
            }
            if !snapshot.file_urls.is_empty() {
                return native_payload_from_snapshot(snapshot);
            }
        }
        // Textual forms are deliberately read only after every image UTI and
        // file representation has been ruled out, so screenshot placeholder
        // text can never suppress an actual image payload.
        let html_type = NSString::alloc(nil).init_str("public.html").autorelease();
        let rtf_type = NSString::alloc(nil).init_str("public.rtf").autorelease();
        native_payload_from_snapshot(NativePasteboardSnapshot {
            html: native_string_value(pasteboard.stringForType(html_type)),
            rich_text: native_rtf_string_value(pasteboard.dataForType(rtf_type)),
            text: native_string_value(pasteboard.stringForType(NSPasteboardTypeString)),
            ..Default::default()
        })
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_native_pasteboard() -> Option<ClipboardPayload> {
    None
}

fn fixture_png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        .expect("embedded PNG fixture is valid base64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use image::{ImageBuffer, Rgba};

    fn fixture_jpeg_bytes() -> Vec<u8> {
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            2,
            3,
            Rgba([0x11, 0x55, 0x99, 0xff]),
        ))
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .expect("encode JPEG fixture");
        encoded.into_inner()
    }

    #[test]
    fn bounded_decode_downsamples_before_render_image_creation() {
        let source = ImageBuffer::from_pixel(4096, 4096, Rgba([0x11, 0x22, 0x33, 0xff]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("fixture PNG should encode");
        let image = BudgetedImageCache::decode_bounded(encoded.get_ref(), 1024 * 1024)
            .expect("bounded decode should succeed");
        let size = image.size(0);
        assert!(u64::from(size.width) * u64::from(size.height) * 4 <= 1024 * 1024);
        assert!(u32::from(size.width) < 4096 || u32::from(size.height) < 4096);
    }

    #[test]
    fn viewport_proxy_uses_device_pixels_without_retina_downsampling() {
        assert_eq!(proxy_max_edge_for_viewport(680.0, 2.0), 1360);
        assert_eq!(proxy_max_edge_for_viewport(680.0, 1.0), 680);
    }

    #[test]
    fn resource_import_rejects_jpeg_bytes_renamed_as_png() {
        // Without an actual-format check, this passed as an ImageFormat::Png
        // because dimensions are guessed from the JPEG bytes. The later GPUI
        // cache then receives a .png path with JPEG data and fails after the
        // durable resource has already been associated.
        let path = std::env::temp_dir().join(format!(
            "joplin-lite-renamed-jpeg-{}.png",
            uuid::Uuid::new_v4()
        ));
        let mut jpeg = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            2,
            3,
            Rgba([0x31, 0x52, 0x73, 0xff]),
        ))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .expect("encode JPEG fixture");
        std::fs::write(&path, jpeg.into_inner()).expect("write renamed fixture");

        let result = ResourceImport::from_path(&path);
        let _ = std::fs::remove_file(path);
        assert!(matches!(result, Err(ResourceImportError::InvalidImage)));
    }

    #[test]
    fn path_attachment_import_keeps_a_descriptor_instead_of_a_payload_vec() {
        // A Finder-selected PDF may be tens of megabytes.  The intake object
        // must retain a checked descriptor plus its exact length, not the
        // entire file again before the repository's fixed-buffer stream can
        // stage it.
        let path = std::env::temp_dir().join(format!(
            "joplin-lite-streaming-attachment-{}.pdf",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, vec![0x4a; 3 * 64 * 1024 + 7]).expect("write attachment fixture");

        let import = ResourceImport::from_path(&path).expect("open checked attachment source");
        let _ = std::fs::remove_file(path);

        assert!(matches!(
            import.source,
            ResourceSource::File { size, .. } if size == 3 * 64 * 1024 + 7
        ));
    }

    #[test]
    fn viewport_proxy_ignores_invalid_scale_and_width() {
        assert_eq!(proxy_max_edge_for_viewport(0.0, 2.0), 1);
        assert_eq!(proxy_max_edge_for_viewport(680.0, 0.0), 1);
        assert_eq!(proxy_max_edge_for_viewport(f32::NAN, 2.0), 1);
    }

    #[test]
    fn quantized_proxy_edge_saturates_at_u32_max() {
        assert_eq!(quantize_proxy_edge(u32::MAX - 63), u32::MAX - 63);
        assert_eq!(quantize_proxy_edge(u32::MAX), u32::MAX);
        assert_eq!(quantize_proxy_edge_with_natural_max(3025, Some(3025)), 3025);
        assert_eq!(quantize_proxy_edge_with_natural_max(3025, Some(0)), 3072);
    }

    #[test]
    fn request_edge_change_clears_deferred_retry_gates() {
        let resource = Resource::from(PathBuf::from("/tmp/request-edge-change.png"));
        let key = hash(&resource);
        let mut cache = BudgetedImageCache::new(1024);
        cache.request_edge(&resource, 1360);
        cache.deferred.insert(key);
        cache.promotion_deferred.insert(key);
        cache.promotion_failures.insert((key, 1360));

        cache.request_edge(&resource, 680);

        assert!(!cache.deferred.contains(&key));
        assert!(!cache.promotion_deferred.contains(&key));
        assert!(
            !cache
                .promotion_failures
                .iter()
                .any(|(failed_key, _)| *failed_key == key)
        );
    }

    #[test]
    fn request_edge_with_natural_max_keeps_retina_tier_and_small_prefetch_tier() {
        let resource = Resource::from(PathBuf::from("/tmp/request-edge-tiers.png"));
        let key = hash(&resource);
        let mut cache = BudgetedImageCache::new(DECODED_IMAGE_CACHE_BUDGET);

        cache.request_edge_with_natural_max(&resource, 1360, Some(1600));
        assert_eq!(cache.requested_edges.get(&key), Some(&1408));

        cache.request_edge_with_natural_max(&resource, 512, Some(1600));
        assert_eq!(cache.requested_edges.get(&key), Some(&512));
    }

    #[test]
    fn bounded_decode_respects_viewport_device_pixel_edge() {
        let source = ImageBuffer::from_pixel(4096, 4096, Rgba([0x11, 0x22, 0x33, 0xff]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("fixture PNG should encode");
        let image = BudgetedImageCache::decode_bounded_with_max_edge(
            encoded.get_ref(),
            DECODED_IMAGE_CACHE_BUDGET,
            1360,
        )
        .expect("bounded decode should succeed");
        let size = image.size(0);
        assert_eq!(
            (u32::from(size.width), u32::from(size.height)),
            (1360, 1360)
        );
    }

    #[test]
    fn bounded_svg_uses_target_pixmap_for_huge_source_and_preserves_ratio() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10000" height="10000"><rect width="10000" height="10000" fill="#123456"/></svg>"##;
        let image = BudgetedImageCache::decode_bounded(svg, DECODED_IMAGE_CACHE_BUDGET)
            .expect("SVG proxy should render without allocating the source canvas");
        let size = image.size(0);
        let width = u32::from(size.width);
        let height = u32::from(size.height);
        assert!(width.max(height) <= DEFAULT_PROXY_MAX_EDGE);
        assert!(u64::from(width) * u64::from(height) * 4 <= DECODED_IMAGE_CACHE_BUDGET as u64);
        assert_eq!((width, height), (1600, 1600));
        let ratio = width as f64 / height as f64;
        assert!((ratio - 1.0).abs() < 0.001);
        assert_eq!(
            BudgetedImageCache::bounded_svg_dimensions(
                10_000.0,
                10_000.0,
                DECODED_IMAGE_CACHE_BUDGET,
                DEFAULT_PROXY_MAX_EDGE,
            )
            .expect("bounded SVG geometry"),
            (1600, 1600)
        );
    }

    #[test]
    fn bounded_svg_target_geometry_keeps_wide_aspect_ratio() {
        let (width, height) = BudgetedImageCache::bounded_svg_dimensions(
            10_000.0,
            5_000.0,
            DECODED_IMAGE_CACHE_BUDGET,
            DEFAULT_PROXY_MAX_EDGE,
        )
        .expect("wide SVG geometry");
        let ratio = width as f64 / height as f64;
        assert!((ratio - 2.0).abs() < 0.002);
        assert!(width.max(height) <= DEFAULT_PROXY_MAX_EDGE);
        assert!(u64::from(width) * u64::from(height) * 4 <= DECODED_IMAGE_CACHE_BUDGET as u64);
    }

    #[test]
    fn bounded_animation_rechecks_actual_frame_budget_after_resize() {
        let frame = image::Frame::new(ImageBuffer::from_pixel(
            4,
            4,
            Rgba([0x11, 0x22, 0x33, 0xff]),
        ));
        let mut encoded = std::io::Cursor::new(Vec::new());
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut encoded);
            encoder
                .encode_frames([frame.clone(), frame])
                .expect("fixture GIF should encode");
        }
        let image = BudgetedImageCache::decode_bounded(encoded.get_ref(), 17 * 4)
            .expect("bounded GIF should fit after resize");
        let decoded_bytes = (0..image.frame_count())
            .map(|index| {
                let size = image.size(index);
                u64::from(size.width) * u64::from(size.height) * 4
            })
            .sum::<u64>();
        assert!(decoded_bytes <= 17 * 4);
        assert_eq!(
            image.frame_count(),
            1,
            "MVP does not retain unused GIF frames"
        );
    }

    #[test]
    fn merged_clipboard_representations_keep_gpui_image_over_native_html_text() {
        let native = ClipboardPayload {
            html: Some("<img src=\"https://example.invalid/photo.png\">".into()),
            text: Some("图像占位符".into()),
            ..Default::default()
        };
        let gpui = ClipboardPayload::fixture_with_png_and_text("fallback");
        let merged = resolve_clipboard_payload(Some(native), Some(gpui)).expect("merged payload");
        assert!(matches!(
            classify_clipboard(merged),
            PasteIntent::Image { .. }
        ));
    }

    #[test]
    fn rejected_native_image_blocks_gpui_placeholder_and_duplicate_image_fallback() {
        let native = ClipboardPayload {
            native_image_rejected: true,
            text: Some("截图占位文本".into()),
            ..ClipboardPayload::default()
        };
        let gpui = ClipboardPayload::fixture_with_png_and_text("GPUI 伴随文本");
        let merged = resolve_clipboard_payload(Some(native), Some(gpui))
            .expect("native rejection remains an explicit payload state");
        assert!(merged.native_image_rejected);
        assert!(matches!(
            classify_clipboard(merged),
            PasteIntent::Unsupported
        ));
    }

    #[test]
    fn html_data_uri_classifier_retains_bounded_encoded_bytes_for_the_worker() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(fixture_png_bytes());
        let intent = classify_clipboard(ClipboardPayload {
            html: Some(format!("<IMG src='DATA:IMAGE/PNG;BASE64,{encoded}'>")),
            ..ClipboardPayload::default()
        });
        let PasteIntent::EncodedImage { payload } = intent else {
            panic!("the callback must retain an encoded descriptor, not decode an ImagePayload");
        };
        assert_eq!(payload.encoded, encoded);
        assert_eq!(payload.format, ImageFormat::Png);

        let over_bound = "A".repeat(MAX_DATA_URI_ENCODED_BYTES + 1);
        assert!(matches!(
            classify_clipboard(ClipboardPayload {
                html: Some(format!("<img src='data:image/png;base64,{over_bound}'>")),
                ..ClipboardPayload::default()
            }),
            PasteIntent::Unsupported
        ));
    }

    #[test]
    fn retry_failed_resource_transitions_back_to_loading() {
        let mut store = ImageStore::for_test();
        let id = store.insert_with_format(
            ImageMetadata::new("retryable", 1, 1),
            fixture_png_bytes(),
            ImageFormat::Png,
        );
        assert!(store.finish_failed_decode(id, "test"));
        assert_eq!(store.node_state(id), ImageNodeState::Failed);
        assert!(store.retry_resource("retryable"));
        assert_eq!(store.node_state(id), ImageNodeState::Loading);
        assert!(!store.retry_resource("retryable"));
    }

    #[test]
    fn managed_resource_keeps_extension_and_failure_state_observable() {
        let mut store = ImageStore::for_test();
        let id = store.insert_with_format(
            ImageMetadata::new("image-resource", 1, 1),
            vec![0, 1, 2],
            ImageFormat::Png,
        );
        let path = store
            .source_path_for_resource("image-resource")
            .expect("managed path");
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
        assert_eq!(store.compressed_len(id), None);
        assert!(std::fs::read(path).is_ok());
        store.finish_failed_decode(id, "fixture");
        assert_eq!(store.node_state(id), ImageNodeState::Failed);
        assert!(store.can_retry(id));
    }

    #[test]
    fn failed_materialization_retains_bytes_for_retry() {
        let mut store = ImageStore::for_test();
        store.resource_root = PathBuf::from("/dev/null/joplin-lite-images");
        let id = store.insert_with_format(
            ImageMetadata::new("failed-resource", 1, 1),
            vec![1, 2, 3],
            ImageFormat::Png,
        );
        assert_eq!(store.node_state(id), ImageNodeState::Failed);
        assert_eq!(store.compressed_len(id), Some(3));
        assert!(store.can_retry(id));
    }

    #[test]
    fn production_clipboard_classification_moves_bounded_image_bytes() {
        let bytes = vec![0x7f; app_lite_core::MAX_IMAGE_BYTES];
        let pointer = bytes.as_ptr();
        let payload = ClipboardPayload {
            images: vec![ImagePayload::new(ImageFormat::Png, bytes)],
            ..ClipboardPayload::default()
        };
        let intent = classify_clipboard(payload);
        let PasteIntent::Image { payload } = intent else {
            panic!("image payload should remain image-first");
        };
        assert_eq!(payload.bytes.as_ptr(), pointer);
    }

    #[test]
    fn production_clipboard_classification_rejects_over_limit_image_without_text_fallback() {
        // The owned-buffer move contract applies only to a valid inline image.
        // A payload over the explicit 10 MiB image bound must not become a
        // placeholder string simply because the same pasteboard also exposed
        // text.
        let intent = classify_clipboard(ClipboardPayload {
            images: vec![ImagePayload::new(
                ImageFormat::Png,
                vec![0x7f; app_lite_core::MAX_IMAGE_BYTES + 1],
            )],
            text: Some("不能降级成文本".into()),
            ..ClipboardPayload::default()
        });
        assert!(matches!(intent, PasteIntent::Unsupported));
    }

    #[gpui::test]
    fn production_cache_releases_offscreen_inflight_slot(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-lifecycle-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("cache lifecycle fixture directory");
        let source_a = root.join("a.png");
        let source_b = root.join("b.png");
        std::fs::write(&source_a, fixture_png_bytes()).expect("fixture a");
        std::fs::write(&source_b, fixture_png_bytes()).expect("fixture b");
        let resource_a = Resource::from(source_a.clone());
        let resource_b = Resource::from(source_b.clone());
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 48 * 1024 * 1024));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_a]);
                assert!(cache.load(&resource_a, window, entity_cx).is_none());
                assert!(cache.reserved_bytes_for_test() > 0);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
                // The image leaves the viewport before its background task
                // completes. Completion must still reclaim the in-flight slot.
                cache.set_visible_resources(std::iter::empty());
            });
        });
        window.run_until_parked();
        assert_eq!(
            window.read(|app| cache.read(app).in_flight_for_test()),
            0,
            "offscreen completion must release its admission slot"
        );
        assert_eq!(
            window.read(|app| cache.read(app).reserved_bytes_for_test()),
            0
        );
        assert!(
            window.read(|app| cache.read(app).drop_image_calls_for_test()) > 0,
            "offscreen completion must release the RenderImage through drop_image"
        );

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_b]);
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert_eq!(cache.in_flight_for_test(), 1);
            });
        });
        window.run_until_parked();
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_drops_loaded_offscreen_entries_before_settle(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-offscreen-loaded-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("offscreen cache fixture directory");
        let source = root.join("image.png");
        std::fs::write(&source, fixture_png_bytes()).expect("offscreen fixture");
        let resource = Resource::from(source.clone());
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 48 * 1024 * 1024));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource]);
                assert!(cache.load(&resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource, window, entity_cx).is_some());
                assert!(cache.used_bytes() > 0);
                assert!(cache.peak_accounted_bytes() > 0);
                cache.set_visible_resources(std::iter::empty());
                cache.evict_offscreen(window, entity_cx);
                assert_eq!(cache.used_bytes(), 0);
                assert_eq!(cache.len(), 0);
                assert!(cache.drop_image_calls_for_test() > 0);
                assert!(cache.is_settled());
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_promotes_visible_proxy_without_downgrade_churn(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-promotion-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("promotion fixture directory");
        let source = root.join("image.png");
        let image = ImageBuffer::from_pixel(1600, 900, Rgba([0x11, 0x22, 0x33, 0xff]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("promotion fixture should encode");
        std::fs::write(&source, encoded.into_inner()).expect("promotion fixture should write");
        let resource = Resource::from(source.clone());
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource]);
                cache.request_edge(&resource, 680);
                assert!(cache.load(&resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        let low_edge = window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("low-resolution proxy should settle")
                    .expect("low-resolution proxy should decode");
                let size = image.size(0);
                u32::from(size.width).max(u32::from(size.height))
            })
        });
        assert!(
            low_edge >= 680 && low_edge < 1360,
            "initial proxy edge was {low_edge}"
        );

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.request_edge(&resource, 1360);
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("promotion must keep the old proxy drawable")
                    .expect("old proxy must remain successful while promoting");
                let size = image.size(0);
                assert_eq!(
                    u32::from(size.width).max(u32::from(size.height)),
                    low_edge,
                    "promotion must not flash a placeholder or change size in flight"
                );
                assert_eq!(cache.in_flight_for_test(), 1);
                assert!(cache.reserved_bytes_for_test() > 0);
                assert!(cache.accounted_bytes_for_test() > cache.reserved_bytes_for_test());
                assert!(cache.peak_accounted_bytes() >= cache.accounted_bytes_for_test());
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        window.run_until_parked();
        let promoted_edge = window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("promoted proxy should settle")
                    .expect("promoted proxy should decode");
                let size = image.size(0);
                u32::from(size.width).max(u32::from(size.height))
            })
        });
        assert!(
            promoted_edge >= 1360,
            "promoted proxy edge was {promoted_edge}"
        );

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.request_edge(&resource, 680);
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("shrinking viewport should reuse promoted proxy")
                    .expect("promoted proxy should remain valid");
                let size = image.size(0);
                assert_eq!(
                    u32::from(size.width).max(u32::from(size.height)),
                    promoted_edge
                );
                assert_eq!(cache.in_flight_for_test(), 0);
                assert!(cache.peak_accounted_bytes() <= cache.budget_bytes());
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_natural_edge_cap_avoids_non_multiple_promotion(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-natural-edge-cap-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("natural edge fixture directory");
        let source = root.join("image.png");
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            3025,
            1,
            Rgba([0x11, 0x22, 0x33, 0xff]),
        ))
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("natural edge fixture should encode");
        std::fs::write(&source, encoded.into_inner()).expect("natural edge fixture should write");
        let resource = Resource::from(source.clone());
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource]);
                cache.request_edge_with_natural_max(&resource, 4096, Some(3025));
                assert!(cache.load(&resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("natural-edge proxy should settle")
                    .expect("natural-edge proxy should decode");
                let size = image.size(0);
                assert_eq!(u32::from(size.width).max(u32::from(size.height)), 3025);
                assert_eq!(cache.in_flight_for_test(), 0);
                cache.set_visible_resources(std::iter::empty());
                cache.set_visible_resources([&resource]);
                cache.request_edge_with_natural_max(&resource, 4096, Some(3025));
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("natural-edge proxy should remain drawable")
                    .expect("natural-edge proxy should remain successful");
                let size = image.size(0);
                assert_eq!(u32::from(size.width).max(u32::from(size.height)), 3025);
                assert_eq!(cache.in_flight_for_test(), 0);
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_keeps_old_proxy_when_promotion_fails(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-promotion-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("promotion failure fixture directory");
        let source = root.join("image.png");
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            1600,
            900,
            Rgba([0x11, 0x22, 0x33, 0xff]),
        ))
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("promotion failure fixture should encode");
        let bytes = encoded.into_inner();
        std::fs::write(&source, &bytes).expect("promotion failure fixture should write");
        let resource = Resource::from(source.clone());
        let cache =
            cx.update(|app| BudgetedImageCache::new_entity(app, DECODED_IMAGE_CACHE_BUDGET));
        let window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource]);
                cache.request_edge(&resource, 680);
                assert!(cache.load(&resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        let old_edge = window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("initial proxy should settle")
                    .expect("initial proxy should decode");
                let size = image.size(0);
                u32::from(size.width).max(u32::from(size.height))
            })
        });

        std::fs::remove_file(&source).expect("promotion source should be removable");
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.request_edge(&resource, 1360);
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("failed promotion must keep old proxy")
                    .expect("old proxy must remain successful");
                let size = image.size(0);
                assert_eq!(u32::from(size.width).max(u32::from(size.height)), old_edge);
                assert!(cache.reserved_bytes_for_test() > 0);
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("failed promotion should settle back to old proxy")
                    .expect("old proxy should survive promotion failure");
                let size = image.size(0);
                assert_eq!(u32::from(size.width).max(u32::from(size.height)), old_edge);
                assert_eq!(cache.in_flight_for_test(), 0);
                assert_eq!(cache.reserved_bytes_for_test(), 0);
            });
        });

        std::fs::write(&source, &bytes).expect("promotion retry source should be restored");
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.request_edge(&resource, 1536);
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("retry should keep drawing old proxy")
                    .expect("old proxy should remain while retrying");
                let size = image.size(0);
                assert_eq!(u32::from(size.width).max(u32::from(size.height)), old_edge);
                assert_eq!(cache.in_flight_for_test(), 1);
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&resource, window, entity_cx)
                    .expect("promotion retry should settle")
                    .expect("promotion retry should decode");
                let size = image.size(0);
                assert!(u32::from(size.width).max(u32::from(size.height)) > old_edge);
                assert_eq!(cache.in_flight_for_test(), 0);
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_retries_same_edge_after_budget_is_freed(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-promotion-budget-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("promotion budget fixture directory");
        let filler_path = root.join("filler.png");
        let target_path = root.join("target.png");
        let mut filler = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            1600,
            1600,
            Rgba([0x11, 0x22, 0x33, 0xff]),
        ))
        .write_to(&mut filler, image::ImageFormat::Png)
        .expect("filler fixture should encode");
        let mut target = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            4096,
            4096,
            Rgba([0x44, 0x55, 0x66, 0xff]),
        ))
        .write_to(&mut target, image::ImageFormat::Png)
        .expect("target fixture should encode");
        std::fs::write(&filler_path, filler.into_inner()).expect("filler fixture should write");
        std::fs::write(&target_path, target.into_inner()).expect("target fixture should write");
        let filler_resource = Resource::from(filler_path.clone());
        let target_resource = Resource::from(target_path.clone());
        let budget = 16 * 1024 * 1024;
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, budget));
        let window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&filler_resource, &target_resource]);
                cache.request_edge(&filler_resource, 1600);
                assert!(cache.load(&filler_resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&filler_resource, window, entity_cx).is_some());
                cache.request_edge(&target_resource, 3000);
                assert!(cache.load(&target_resource, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        let first_edge = window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&target_resource, window, entity_cx)
                    .expect("budgeted target should settle")
                    .expect("budgeted target should decode");
                let size = image.size(0);
                u32::from(size.width).max(u32::from(size.height))
            })
        });
        assert!(
            first_edge < 3000,
            "target should be budget-limited initially"
        );

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&target_resource]);
                cache.evict_offscreen(window, entity_cx);
                assert!(cache.used_bytes() < budget);
                let image = cache
                    .load(&target_resource, window, entity_cx)
                    .expect("target should stay drawable while retry is scheduled")
                    .expect("target proxy should remain valid");
                let size = image.size(0);
                assert_eq!(
                    u32::from(size.width).max(u32::from(size.height)),
                    first_edge
                );
                assert_eq!(cache.in_flight_for_test(), 1);
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let image = cache
                    .load(&target_resource, window, entity_cx)
                    .expect("same edge promotion should settle")
                    .expect("same edge promotion should decode");
                let size = image.size(0);
                assert!(u32::from(size.width).max(u32::from(size.height)) > first_edge);
                assert_eq!(cache.in_flight_for_test(), 0);
                assert!(cache.peak_accounted_bytes() <= budget);
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_reserves_remaining_budget_with_retained_image(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-reservation-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("reservation fixture directory");
        let source_a = root.join("a.png");
        let source_b = root.join("b.png");
        std::fs::write(&source_a, fixture_png_bytes()).expect("fixture a");
        std::fs::write(&source_b, fixture_png_bytes()).expect("fixture b");
        let resource_a = Resource::from(source_a.clone());
        let resource_b = Resource::from(source_b.clone());
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 8));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_a]);
                assert!(cache.load(&resource_a, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_a, window, entity_cx).is_some());
                assert_eq!(cache.used_bytes(), 4);
                cache.set_visible_resources([&resource_a, &resource_b]);
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert_eq!(cache.reserved_bytes_for_test(), 4);
                assert_eq!(cache.accounted_bytes_for_test(), 8);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_b, window, entity_cx).is_some());
                assert_eq!(cache.used_bytes(), 8);
                assert_eq!(cache.reserved_bytes_for_test(), 0);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_admits_small_proxy_with_less_than_four_mib_remaining(
        cx: &mut TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-adaptive-floor-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("adaptive floor fixture directory");
        let source_a = root.join("large.png");
        let source_b = root.join("small.png");
        let large = ImageBuffer::from_pixel(1320, 1321, Rgba([0x11, 0x22, 0x33, 0xff]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(large)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("large fixture should encode");
        std::fs::write(&source_a, encoded.into_inner()).expect("large fixture should write");
        std::fs::write(&source_b, fixture_png_bytes()).expect("small fixture should write");
        let resource_a = Resource::from(source_a.clone());
        let resource_b = Resource::from(source_b.clone());
        let budget = 4 * 1024 * 1024;
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, budget));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_a]);
                assert!(cache.load(&resource_a, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_a, window, entity_cx).is_some());
                let remaining = budget.saturating_sub(cache.used_bytes());
                assert!(remaining >= 4);
                assert!(remaining < CONSERVATIVE_PROXY_RESERVATION);
                cache.set_visible_resources([&resource_a, &resource_b]);
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert!(cache.reserved_bytes_for_test() >= 4);
                assert!(cache.accounted_bytes_for_test() <= budget);
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_b, window, entity_cx).is_some());
                assert_eq!(cache.reserved_bytes_for_test(), 0);
                assert!(cache.used_bytes() <= budget);
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_defers_when_remainder_cannot_hold_one_rgba_pixel(cx: &mut TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-subpixel-remainder-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("subpixel fixture directory");
        let source_a = root.join("a.png");
        let source_b = root.join("b.png");
        std::fs::write(&source_a, fixture_png_bytes()).expect("fixture a");
        std::fs::write(&source_b, fixture_png_bytes()).expect("fixture b");
        let resource_a = Resource::from(source_a.clone());
        let resource_b = Resource::from(source_b.clone());
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 7));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_a, &resource_b]);
                assert!(cache.load(&resource_a, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_a, window, entity_cx).is_some());
                assert_eq!(cache.used_bytes(), 4);
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert_eq!(cache.in_flight_for_test(), 0);
                assert_eq!(cache.reserved_bytes_for_test(), 0);
                assert_eq!(cache.deferred_len_for_test(), 1);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[gpui::test]
    fn production_cache_defers_capacity_and_retries_after_visible_set_change(
        cx: &mut TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lite-cache-deferred-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("deferred fixture directory");
        let source_a = root.join("a.png");
        let source_b = root.join("b.png");
        std::fs::write(&source_a, fixture_png_bytes()).expect("fixture a");
        std::fs::write(&source_b, fixture_png_bytes()).expect("fixture b");
        let resource_a = Resource::from(source_a.clone());
        let resource_b = Resource::from(source_b.clone());
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 4));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource_a]);
                assert!(cache.load(&resource_a, window, entity_cx).is_none());
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_a, window, entity_cx).is_some());
                assert_eq!(cache.used_bytes(), 4);
                cache.set_visible_resources([&resource_a, &resource_b]);
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert_eq!(cache.deferred_len_for_test(), 1);
                cache.set_visible_resources([&resource_b]);
                // Changing the visible working set clears Deferred and allows
                // exactly one bounded retry instead of a permanent error.
                assert!(cache.load(&resource_b, window, entity_cx).is_none());
                assert_eq!(cache.in_flight_for_test(), 1);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let loaded = cache.load(&resource_b, window, entity_cx);
                assert!(loaded.is_some(), "deferred resource should be admitted");
                assert_eq!(cache.used_bytes(), 4);
                assert_eq!(cache.reserved_bytes_for_test(), 0);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        assert!(
            window.read(|app| cache.read(app).drop_image_calls_for_test()) > 0,
            "visible-set change must evict the offscreen RenderImage through drop_image"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_completion_cannot_mutate_new_retry_generation() {
        let resource = Resource::from(PathBuf::from("/tmp/task6-generation.png"));
        let key = hash(&resource);
        let mut cache = BudgetedImageCache::new(16);
        let error = ImageCacheError::from(anyhow!("old decode failed"));

        // Paint harvested generation one and released its reservation. The
        // user then invalidated that result and immediately started generation
        // two for the same resource key.
        cache.next_generation = 1;
        cache.entries.insert(
            key,
            CachedTexture {
                item: ImageCacheItem::Loaded(Err(error.clone())),
                decoded_bytes: 0,
                generation: 1,
                proxy_max_edge: 1,
            },
        );
        cache.entries.remove(&key);
        cache.next_generation = 2;
        cache.reservations.insert(key, (2, 16));
        cache.reserved_bytes = 16;
        cache.in_flight = 1;
        cache.entries.insert(
            key,
            CachedTexture {
                item: ImageCacheItem::Loaded(Err(error)),
                decoded_bytes: 0,
                generation: 2,
                proxy_max_edge: 1,
            },
        );

        // The late generation-one callback must fail the production finalize
        // path and leave generation two's entry and reservation untouched.
        assert!(matches!(
            cache.finalize_completion(key, 1),
            CompletionState::Stale { harvested: false }
        ));
        assert_eq!(cache.entries.get(&key).unwrap().generation, 2);
        assert_eq!(cache.reserved_bytes, 16);
        assert_eq!(cache.in_flight, 1);
        assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());

        assert!(matches!(
            cache.finalize_completion(key, 2),
            CompletionState::Harvested(_)
        ));
        assert!(!cache.entries.contains_key(&key));
        assert_eq!(cache.reserved_bytes, 0);
        assert_eq!(cache.in_flight, 0);
    }

    #[gpui::test]
    fn pending_retry_restarts_when_invalidated_task_releases_last_slot(cx: &mut TestAppContext) {
        let resource = Resource::from(std::env::temp_dir().join(format!(
            "joplin-lite-missing-retry-{}.png",
            uuid::Uuid::new_v4()
        )));
        let cache = cx.update(|app| BudgetedImageCache::new_entity(app, 4 * 1024 * 1024));
        let mut window = cx.add_empty_window();

        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                cache.set_visible_resources([&resource]);
                assert!(cache.load(&resource, window, entity_cx).is_none());
                assert_eq!(cache.in_flight_for_test(), 1);
                cache.invalidate(&resource, window, entity_cx);
                // The immediate retry is intentionally still blocked by the
                // old shared task. Its completion owns the pending restart.
                assert!(cache.load(&resource, window, entity_cx).is_none());
                assert_eq!(cache.in_flight_for_test(), 1);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
        window.run_until_parked();
        window.update(|window, app| {
            cache.update(app, |cache, entity_cx| {
                let result = cache.load(&resource, window, entity_cx);
                assert!(
                    matches!(result, Some(Err(_))),
                    "pending retry must reach Failed"
                );
                assert_eq!(cache.in_flight_for_test(), 0);
                assert_eq!(cache.reserved_bytes_for_test(), 0);
                assert!(cache.accounted_bytes_for_test() <= cache.budget_bytes());
            });
        });
    }

    #[test]
    fn managed_path_insert_copies_original_without_retaining_compressed_bytes() {
        let source_path = std::env::temp_dir().join(format!(
            "joplin-lite-path-source-{}.png",
            uuid::Uuid::new_v4()
        ));
        let original = fixture_png_bytes();
        std::fs::write(&source_path, &original).expect("source image should be writable");
        let mut store = ImageStore::for_test();
        let id = store
            .insert_from_path(
                ImageMetadata::new("path-resource", 1, 1),
                &source_path,
                ImageFormat::Png,
            )
            .expect("path image should be copied into managed storage");
        let managed = store
            .source_path_for_resource("path-resource")
            .expect("managed source path");
        assert_ne!(managed, source_path.as_path());
        assert_eq!(std::fs::read(managed).expect("managed image"), original);
        assert_eq!(store.compressed_len(id), None);
        let _ = std::fs::remove_file(source_path);
    }

    #[test]
    fn session_private_materialized_sources_are_removed_when_store_drops() {
        let source = fixture_png_bytes();
        let managed = {
            let mut store = ImageStore::for_test();
            store
                .insert_durable_reader(
                    ImageMetadata::new("drop-private-source", 1, 1),
                    std::io::Cursor::new(source),
                    ImageFormat::Png,
                )
                .expect("stream source into private cache");
            store
                .source_path_for_resource("drop-private-source")
                .expect("managed source")
                .to_owned()
        };
        assert!(
            !managed.exists(),
            "dropping a retained editor must release its private image source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn durable_image_materialization_uses_private_directory_and_leaf_modes() {
        // This invokes the same stream-and-rename route used by persisted
        // library image hydration, rather than a direct fixture write. A
        // permissive umask must not make a note image readable from the shared
        // temporary parent before GPUI decodes it.
        use std::os::unix::fs::PermissionsExt;

        let mut store = ImageStore::for_test();
        store
            .insert_durable_reader(
                ImageMetadata::new("private-durable-image", 1, 1),
                std::io::Cursor::new(fixture_png_bytes()),
                ImageFormat::Png,
            )
            .expect("stream durable image into editor-private cache");
        let root = store.materialization_root();
        let source = store
            .source_path_for_resource("private-durable-image")
            .expect("managed durable source");
        assert_eq!(
            std::fs::metadata(root)
                .expect("private image root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the session image root must not retain default world traversal permissions"
        );
        assert_eq!(
            std::fs::metadata(source)
                .expect("private image source metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the streamed image leaf must not retain default world-read permissions"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_snapshot_prefers_an_owned_image_payload_without_a_temporary_file() {
        let native = native_payload_from_snapshot(NativePasteboardSnapshot {
            images: vec![ImagePayload::new(ImageFormat::Png, fixture_png_bytes())],
            text: Some("图像占位符".into()),
            ..Default::default()
        })
        .expect("native image snapshot should produce a payload");

        assert!(matches!(
            native.images.as_slice(),
            [ImagePayload {
                format: ImageFormat::Png,
                bytes,
                ..
            }] if bytes == &fixture_png_bytes()
        ));
        assert!(native.file_urls.is_empty());
        assert!(native.temporary_files.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_image_uti_seam_collects_bounded_ordered_candidates_before_textual_forms() {
        let mut queried = Vec::new();
        let image = native_image_candidates_from(|format, uti, _remaining_budget| {
            queried.push(uti.to_owned());
            match uti {
                "public.png" => NativeImageRead::Payloads(vec![ImagePayload::new(
                    format,
                    vec![0x89, 0x50, 0x4e],
                )]),
                "public.tiff" => NativeImageRead::Payloads(vec![ImagePayload::new(
                    format,
                    vec![0x49, 0x49, 0x2a],
                )]),
                "public.jpeg" => {
                    NativeImageRead::Payloads(vec![ImagePayload::new(format, fixture_jpeg_bytes())])
                }
                _ => NativeImageRead::Missing,
            }
        });

        assert_eq!(
            queried,
            NATIVE_IMAGE_UTIS
                .iter()
                .map(|(_, uti)| (*uti).to_owned())
                .collect::<Vec<_>>()
        );
        let NativeImageRead::Payloads(images) = image else {
            panic!("the bridge should retain ordered UTI candidates");
        };
        assert_eq!(images.len(), 3);
        assert_eq!(images[0].format, ImageFormat::Png);
        assert_eq!(images[1].format, ImageFormat::Tiff);
        assert_eq!(images[2].format, ImageFormat::Jpeg);
        assert_eq!(
            ResourceImport::from_image_payload(images[2].clone())
                .expect("the third native candidate must really decode")
                .kind,
            ResourceKind::Image {
                format: ImageFormat::Jpeg,
                natural_size: (2, 3),
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_image_uti_seam_rejection_stops_before_textual_fallback() {
        let mut queried = Vec::new();
        let image = native_image_candidates_from(|_, uti, _remaining_budget| {
            queried.push(uti.to_owned());
            NativeImageRead::Rejected
        });

        assert_eq!(
            queried,
            NATIVE_IMAGE_UTIS
                .iter()
                .map(|(_, uti)| (*uti).to_owned())
                .collect::<Vec<_>>()
        );
        assert!(matches!(image, NativeImageRead::Rejected));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_image_uti_seam_uses_a_later_valid_image_after_a_rejected_representation() {
        let mut queried = Vec::new();
        let image = native_image_candidates_from(|format, uti, _remaining_budget| {
            queried.push(uti.to_owned());
            match uti {
                "public.png" => NativeImageRead::Rejected,
                "public.tiff" => {
                    NativeImageRead::Payloads(vec![ImagePayload::new(format, fixture_png_bytes())])
                }
                _ => NativeImageRead::Missing,
            }
        });

        assert_eq!(
            queried,
            NATIVE_IMAGE_UTIS
                .iter()
                .map(|(_, uti)| (*uti).to_owned())
                .collect::<Vec<_>>()
        );
        assert!(matches!(
            image,
            NativeImageRead::Payloads(images)
                if matches!(images.as_slice(), [ImagePayload { format: ImageFormat::Tiff, .. }])
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_image_copy_seam_owns_at_most_ten_mib_without_a_file_handoff() {
        let mut source = fixture_png_bytes();
        let owned = copy_bounded_native_image_bytes(&source)
            .expect("a small native image should copy into a process-owned payload");
        source[0] ^= 0xff;
        assert_eq!(owned, fixture_png_bytes());
        assert!(
            copy_bounded_native_image_bytes(&vec![0_u8; app_lite_core::MAX_IMAGE_BYTES + 1])
                .is_none(),
            "an oversized AppKit NSData must not cross the foreground boundary"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_pasteboard_snapshot_seam_imports_rtf_and_preserves_html() {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSAutoreleasePool;
        use objc::{class, msg_send, sel, sel_impl};

        let rtf = br"{\rtf1\ansi\deff0 {\fonttbl {\f0 Helvetica;}} Hello}";
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);
            let data: id = msg_send![
                class!(NSData),
                dataWithBytes: rtf.as_ptr()
                length: rtf.len()
            ];
            let rich_text = native_rtf_string_value(data).expect("Foundation should import RTF");
            assert_eq!(rich_text.trim(), "Hello");

            let native = native_payload_from_snapshot(NativePasteboardSnapshot {
                html: Some("<img src=\"file:///tmp/photo.png\">".into()),
                rich_text: Some(rich_text),
                text: Some("图像占位符".into()),
                ..Default::default()
            })
            .expect("snapshot with HTML/RTF should produce a payload");
            assert!(
                native
                    .html
                    .as_deref()
                    .is_some_and(|html| html.contains("photo.png"))
            );
            assert_eq!(native.text.as_deref(), Some("图像占位符"));

            let merged = resolve_clipboard_payload(
                Some(native),
                Some(ClipboardPayload::fixture_with_png_and_text("fallback")),
            )
            .expect("native snapshot should merge with GPUI image payload");
            assert!(matches!(
                classify_clipboard(merged),
                PasteIntent::Image { .. }
            ));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn malformed_native_rtf_returns_none_without_breaking_following_image_paste() {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSAutoreleasePool;
        use objc::{class, msg_send, sel, sel_impl};

        let malformed = [0xff_u8, 0x00, 0x7f, 0x01];
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);
            let data: id = msg_send![
                class!(NSData),
                dataWithBytes: malformed.as_ptr()
                length: malformed.len()
            ];
            assert!(
                native_rtf_string_value(data).is_none(),
                "Foundation should reject malformed RTF without an MRC double-release"
            );

            let native = native_payload_from_snapshot(NativePasteboardSnapshot {
                html: Some("<img src=\"file:///tmp/photo.png\">".into()),
                text: Some("图像占位符".into()),
                ..Default::default()
            })
            .expect("valid HTML payload should remain usable after malformed RTF");
            let merged = resolve_clipboard_payload(
                Some(native),
                Some(ClipboardPayload::fixture_with_png_and_text("fallback")),
            )
            .expect("following image paste should resolve");
            assert!(matches!(
                classify_clipboard(merged),
                PasteIntent::Image { .. }
            ));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_resource_load_uses_bounded_imageio_proxy_without_mutating_original() {
        let source = ImageBuffer::from_fn(4031, 3023, |_, y| {
            if y < 1511 {
                Rgba([0x11, 0x22, 0x33, 0xff])
            } else {
                Rgba([0xaa, 0xbb, 0xcc, 0xff])
            }
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("fixture PNG should encode");
        let original = encoded.get_ref().clone();
        let mut store = ImageStore::for_test();
        let id = store.insert_with_format(
            ImageMetadata::new("imageio-proxy", 4031, 3023),
            original.clone(),
            ImageFormat::Png,
        );
        let path = store
            .source_path_for_resource("imageio-proxy")
            .expect("managed source path")
            .to_owned();

        let proxy = BudgetedImageCache::decode_resource_bounded(
            &Resource::from(path.clone()),
            DECODED_IMAGE_CACHE_BUDGET,
        )
        .expect("ImageIO should create a bounded proxy");
        let size = proxy.size(0);
        let width = u32::from(size.width);
        let height = u32::from(size.height);
        assert!(width.max(height) <= 1600, "proxy is {width}x{height}");
        let ratio = width as f32 / height as f32;
        assert!((ratio - 4031.0 / 3023.0).abs() < 0.01);
        let pixels = proxy.as_bytes(0).expect("proxy pixels");
        assert_eq!(&pixels[..4], &[0x33, 0x22, 0x11, 0xff]);
        let last_row = (height as usize - 1) * width as usize * 4;
        assert_eq!(&pixels[last_row..last_row + 4], &[0xcc, 0xbb, 0xaa, 0xff]);
        assert_eq!(store.metadata(id).unwrap().natural_width, 4031);
        assert_eq!(store.metadata(id).unwrap().natural_height, 3023);
        assert_eq!(std::fs::read(&path).expect("managed source"), original);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_card_thumbnail_proxy_round_trip_preserves_unpremultiplied_transparent_png_color() {
        // This exercises the production ImageIO path used by Cards, rather
        // than a synthetic RenderImage. CoreGraphics renders into
        // PremultipliedFirst|Order32Little BGRA; serializing those bytes as
        // ordinary PNG RGBA darkens every half-transparent pixel.
        let source = ImageBuffer::from_fn(4, 1, |x, _| {
            if x == 0 {
                // Transparent RGB is undefined after premultiplication. The
                // exported proxy must make it deterministic rather than leak
                // a stale color under alpha zero.
                Rgba([251, 37, 19, 0])
            } else if x == 1 {
                // At alpha=1, only a full-red source survives 8-bit
                // premultiplication. It must be restored to red rather than
                // remain the almost-black premultiplied component 1.
                Rgba([255, 0, 0, 1])
            } else if x == 2 {
                Rgba([200, 100, 50, 128])
            } else {
                // Alpha 255 is the opaque control: unpremultiplication must
                // be a no-op for existing JPEG-like card fixtures.
                Rgba([20, 110, 220, 255])
            }
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode transparent PNG fixture");
        let root = tempfile::tempdir().expect("temporary card proxy root");
        let proxy = ImageStore::materialize_bounded_thumbnail_proxy_from_verified_reader(
            root.path(),
            &ImageMetadata::new("transparent-card-proxy", 4, 1),
            std::io::Cursor::new(encoded.into_inner()),
            ImageFormat::Png,
            192,
            512 * 1024,
        )
        .expect("ImageIO should materialize a transparent card proxy");
        let round_trip = image::load_from_memory(&std::fs::read(proxy).expect("proxy bytes"))
            .expect("proxy PNG should decode")
            .to_rgba8();
        assert_eq!(
            round_trip.get_pixel(0, 0).0,
            [0, 0, 0, 0],
            "zero-alpha output must not retain an arbitrary premultiplied color"
        );
        assert_eq!(
            round_trip.get_pixel(1, 0).0,
            [255, 0, 0, 1],
            "alpha=1 must restore the surviving premultiplied red component"
        );
        let half = round_trip.get_pixel(2, 0).0;
        assert!(
            half[0].abs_diff(200) <= 2
                && half[1].abs_diff(100) <= 2
                && half[2].abs_diff(50) <= 2
                && half[3].abs_diff(128) <= 1,
            "half-transparent pixel was darkened by premultiplied BGRA export: {half:?}"
        );
        assert_eq!(
            round_trip.get_pixel(3, 0).0,
            [20, 110, 220, 255],
            "alpha=255 must remain an unchanged opaque proxy pixel"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_card_thumbnail_proxy_keeps_opaque_jpeg_color_straight() {
        // Keep the common opaque Cards input on the same ImageIO → PNG proxy
        // path. The alpha=255 branch of unpremultiplication must be a no-op,
        // aside from the JPEG codec's small, bounded color rounding.
        let source = ImageBuffer::from_pixel(8, 8, Rgba([35, 113, 219, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .expect("encode opaque JPEG fixture");
        let root = tempfile::tempdir().expect("temporary card proxy root");
        let proxy = ImageStore::materialize_bounded_thumbnail_proxy_from_verified_reader(
            root.path(),
            &ImageMetadata::new("opaque-card-proxy", 8, 8),
            std::io::Cursor::new(encoded.into_inner()),
            ImageFormat::Jpeg,
            192,
            512 * 1024,
        )
        .expect("ImageIO should materialize an opaque JPEG card proxy");
        let pixel = image::load_from_memory(&std::fs::read(proxy).expect("proxy bytes"))
            .expect("proxy PNG should decode")
            .to_rgba8()
            .get_pixel(0, 0)
            .0;
        assert!(
            pixel[0].abs_diff(35) <= 3
                && pixel[1].abs_diff(113) <= 3
                && pixel[2].abs_diff(219) <= 3
                && pixel[3] == 255,
            "opaque JPEG card proxy must not be modified by alpha restoration: {pixel:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_card_thumbnail_proxy_round_trip_preserves_transparent_svg_color() {
        // `decode_resource_bounded_with_max_edge` routes .svg through resvg
        // rather than CGImageSource. Its tiny-skia pixmap is still
        // premultiplied, so the card PNG exporter must not treat it as a
        // straight-alpha bitmap merely because it bypassed ImageIO.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#c86432" fill-opacity="0.5"/></svg>"##;
        let root = tempfile::tempdir().expect("temporary card proxy root");
        let proxy = ImageStore::materialize_bounded_thumbnail_proxy_from_verified_reader(
            root.path(),
            &ImageMetadata::new("transparent-svg-card-proxy", 4, 4),
            std::io::Cursor::new(svg),
            ImageFormat::Svg,
            192,
            512 * 1024,
        )
        .expect("resvg should materialize a transparent card proxy");
        let pixel = image::load_from_memory(&std::fs::read(proxy).expect("proxy bytes"))
            .expect("proxy PNG should decode")
            .to_rgba8()
            .get_pixel(0, 0)
            .0;
        assert!(
            pixel[0].abs_diff(200) <= 2
                && pixel[1].abs_diff(100) <= 2
                && pixel[2].abs_diff(50) <= 2
                && pixel[3].abs_diff(128) <= 1,
            "transparent SVG card proxy lost its straight color: {pixel:?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_large_resource_decode_reliefs_allocator_once_but_small_does_not() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        mac_pressure::set_test_hook(Some(Box::new(move || {
            observed_calls.fetch_add(1, Ordering::SeqCst);
        })));

        let source = ImageBuffer::from_fn(1536, 1536, |x, y| {
            let value = x.wrapping_mul(73).wrapping_add(y.wrapping_mul(151));
            Rgba([
                value as u8,
                value.rotate_left(7) as u8,
                value.rotate_left(13) as u8,
                0xff,
            ])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("large fixture PNG should encode");
        assert!(encoded.get_ref().len() >= 4 * 1024 * 1024);

        let mut store = ImageStore::for_test();
        let large_id = store.insert_with_format(
            ImageMetadata::new("pressure-large", 1536, 1536),
            encoded.get_ref().clone(),
            ImageFormat::Png,
        );
        let large_path = store
            .source_path_for_resource("pressure-large")
            .expect("large managed path")
            .to_owned();
        BudgetedImageCache::decode_resource_bounded(
            &Resource::from(large_path),
            DECODED_IMAGE_CACHE_BUDGET,
        )
        .expect("large resource should decode");

        let small_id = store.insert_with_format(
            ImageMetadata::new("pressure-small", 1, 1),
            fixture_png_bytes(),
            ImageFormat::Png,
        );
        let small_path = store
            .source_path_for_resource("pressure-small")
            .expect("small managed path")
            .to_owned();
        BudgetedImageCache::decode_resource_bounded(
            &Resource::from(small_path),
            DECODED_IMAGE_CACHE_BUDGET,
        )
        .expect("small resource should decode");

        mac_pressure::set_test_hook(None);
        assert_eq!(store.node_state(large_id), ImageNodeState::Loading);
        assert_eq!(store.node_state(small_id), ImageNodeState::Loading);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
