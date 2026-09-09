//! Image intake and bounded decoded-image lifetime for the native editor.
//!
//! The routing types are intentionally independent of the Markdown donor.  The
//! payload extraction follows the donor's image-first classification and the
//! GPUI 0.2.2 `RetainAllImageCache` loading protocol; only the document model
//! decides when a structural `InsertImage` transaction is committed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use base64::Engine as _;
use futures::FutureExt;
use gpui::{
    App, AppContext, ClipboardEntry, ClipboardItem, Entity, Image, ImageCache, ImageCacheError,
    ImageCacheItem, ImageFormat, RenderImage, Resource, Window, hash,
};
use image::{AnimationDecoder, ImageBuffer, codecs::gif::GifDecoder, imageops::FilterType};
use smallvec::SmallVec;

pub const DECODED_IMAGE_CACHE_BUDGET: usize = 48 * 1024 * 1024;

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

    pub fn from_image(image: &Image) -> Self {
        Self::new(image.format, image.bytes.clone())
    }
}

/// Normalized clipboard representation.  macOS extraction fills `images`
/// before `text`, because GPUI 0.2.2 itself returns public.utf8-plain-text
/// before image UTTypes on that platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardPayload {
    pub images: Vec<ImagePayload>,
    pub file_urls: Vec<PathBuf>,
    pub html: Option<String>,
    pub rich_text: Option<String>,
    pub text: Option<String>,
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
                    payload.images.push(ImagePayload::from_image(&image))
                }
                ClipboardEntry::String(string) => payload.text = Some(string.text().to_owned()),
            }
        }
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
    Image { payload: ImagePayload },
    File { path: PathBuf },
    Text { text: String },
    Unsupported,
}

/// Adapted from donor `components/block/interactions.rs`: classify image
/// payloads before text and never insert an image placeholder string.
pub fn classify_clipboard(payload: &ClipboardPayload) -> PasteIntent {
    if let Some(image) = payload.images.first() {
        return PasteIntent::Image {
            payload: image.clone(),
        };
    }
    if let Some(path) = payload
        .file_urls
        .iter()
        .find(|path| is_supported_image_path(path))
    {
        return PasteIntent::File { path: path.clone() };
    }
    if let Some(html) = payload.html.as_deref()
        && html.to_ascii_lowercase().contains("<img")
    {
        if let Some((format, encoded)) = html.split_once("data:image/").and_then(|(_, value)| {
            let (kind, data) = value.split_once(";base64,")?;
            let format = match kind.to_ascii_lowercase().as_str() {
                "png" => ImageFormat::Png,
                "jpeg" | "jpg" => ImageFormat::Jpeg,
                "gif" => ImageFormat::Gif,
                "webp" => ImageFormat::Webp,
                "bmp" => ImageFormat::Bmp,
                "tiff" => ImageFormat::Tiff,
                _ => return None,
            };
            Some((format, data.split(|ch| ch == '"' || ch == '\'').next()?))
        }) {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                return PasteIntent::Image {
                    payload: ImagePayload::new(format, bytes),
                };
            }
        }
        // An HTML image with an unresolved remote/file source must not become
        // visible markup or a fake PNG node.
        return PasteIntent::Unsupported;
    }
    payload
        .rich_text
        .as_ref()
        .or(payload.text.as_ref())
        .map_or(PasteIntent::Unsupported, |text| PasteIntent::Text {
            text: text.clone(),
        })
}

/// Testable seam for the platform paste action. Native extraction is supplied
/// by the caller, so unit tests never depend on the user's real pasteboard;
/// the ordering remains image/file-first on macOS and GPUI fallback elsewhere.
pub fn resolve_clipboard_payload(
    native: Option<ClipboardPayload>,
    gpui: Option<ClipboardPayload>,
) -> Option<ClipboardPayload> {
    native.or(gpui)
}

pub fn classify_drop(paths: &[PathBuf]) -> PasteIntent {
    paths
        .iter()
        .find(|path| is_supported_image_path(path))
        .cloned()
        .map_or(PasteIntent::Unsupported, |path| PasteIntent::File { path })
}

pub fn image_payload_from_file(path: &Path) -> Option<ImagePayload> {
    let format = path.extension().and_then(|extension| {
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
    })?;
    Some(ImagePayload::new(format, std::fs::read(path).ok()?))
}

fn is_supported_image_path(path: &Path) -> bool {
    path.is_file()
        && path.extension().is_some_and(|extension| {
            matches!(
                extension.to_string_lossy().to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff"
            )
        })
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
        let width = self
            .display_width
            .map_or(available_width, |width| width as f32)
            .max(1.0);
        if self.natural_width == 0 {
            return width;
        }
        (width * self.natural_height as f32 / self.natural_width as f32).max(1.0)
    }

    /// Loading, loaded, and failed nodes deliberately share this geometry.
    pub fn placeholder_height(&self, available_width: f32) -> f32 {
        self.display_height(available_width)
    }
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
            resource_root: std::env::temp_dir().join("joplin-lite-native-images"),
        }
    }
}

impl ImageStore {
    pub fn for_test() -> Self {
        Self::default()
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

    fn insert_inner(
        &mut self,
        metadata: ImageMetadata,
        bytes: Vec<u8>,
        format: Option<ImageFormat>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let extension = format.map_or("img", |format| match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
            ImageFormat::Svg => "svg",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tiff => "tiff",
        });
        let source_path = self
            .resource_root
            .join(format!("{}.{}", metadata.resource_id, extension));
        let resource_ready = std::fs::create_dir_all(&self.resource_root)
            .and_then(|_| std::fs::write(&source_path, &bytes))
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
        let Some((source_path, bytes)) = self.images.get(&id).and_then(|image| {
            image
                .compressed
                .as_ref()
                .map(|bytes| (image.source_path.clone(), bytes.clone()))
        }) else {
            return self
                .images
                .get(&id)
                .is_some_and(|image| image.source_path.is_file());
        };
        let written =
            std::fs::create_dir_all(source_path.parent().unwrap_or_else(|| Path::new(".")))
                .and_then(|_| std::fs::write(&source_path, bytes))
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
}

/// GPUI 0.2.2 image-cache adapter.  It preserves the donor's shared loading
/// task and next-frame notification, while adding exact decoded-byte LRU and
/// `App::drop_image` on eviction.
pub struct BudgetedImageCache {
    budget_bytes: usize,
    used_bytes: usize,
    entries: HashMap<u64, CachedTexture>,
    lru: VecDeque<u64>,
}

impl BudgetedImageCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    /// Create the cache as a GPUI entity and release every retained GPU image
    /// when the entity dies. This mirrors GPUI's donor cache lifecycle instead
    /// of relying on `Arc<RenderImage>` drops to clean the platform atlas.
    pub fn new_entity(cx: &mut App, budget_bytes: usize) -> Entity<Self> {
        let entity = cx.new(|_| Self::new(budget_bytes));
        cx.observe_release(&entity, |cache, cx| {
            for (_, mut entry) in std::mem::take(&mut cache.entries) {
                if let Some(Ok(image)) = entry.item.get() {
                    cx.drop_image(image, None);
                }
            }
            cache.lru.clear();
            cache.used_bytes = 0;
        })
        .detach();
        entity
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }
    pub fn len(&self) -> usize {
        self.entries.len()
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

    /// Adapted from GPUI 0.2.2 `Image::to_image_data`: decode each format,
    /// resize while still in image buffers, convert RGBA to GPUI BGRA, and
    /// construct exactly one bounded `RenderImage`.
    fn decode_bounded(
        bytes: &[u8],
        budget_bytes: usize,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        let guessed_format = image::guess_format(bytes);
        let is_svg = guessed_format.is_err();
        let mut frames = if let Ok(format) = guessed_format {
            if format == image::ImageFormat::Gif {
                GifDecoder::new(std::io::Cursor::new(bytes))?
                    .into_frames()
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                vec![image::Frame::new(
                    image::load_from_memory_with_format(bytes, format)?.into_rgba8(),
                )]
            }
        } else {
            let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
                .map_err(|error| anyhow!(error.to_string()))?;
            let svg_size = tree.size();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(
                svg_size.width().ceil() as u32,
                svg_size.height().ceil() as u32,
            )
            .ok_or_else(|| anyhow!("SVG renderer returned invalid size"))?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
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
        if decoded_bytes > budget_bytes {
            let max_pixels = budget_bytes / 4;
            if max_pixels < frames.len() {
                return Err(ImageCacheError::from(anyhow!(
                    "decoded animation exceeds minimum budget"
                )));
            }
            let scale = (max_pixels as f64 / (decoded_bytes / 4) as f64).sqrt();
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
        // GPUI's bitmap path expects BGRA (the same conversion used by
        // platform.rs::Image::to_image_data). resvg's tiny-skia pixmap is
        // already in the platform-native premultiplied order, so do not
        // apply the bitmap swap to SVG output.
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

    fn evict_until_fit(&mut self, needed: usize, cx: &mut App, window: &mut Window) {
        while self.used_bytes.saturating_add(needed) > self.budget_bytes {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if let Some(mut entry) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                if let Some(Ok(image)) = entry.item.get() {
                    cx.drop_image(image, Some(window));
                }
            }
        }
    }
}

impl ImageCache for BudgetedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        if let Some(mut entry) = self.entries.remove(&key) {
            self.lru.retain(|value| *value != key);
            let result = entry.item.get();
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
                    cx.drop_image(image.clone(), Some(window));
                    entry.item = ImageCacheItem::Loaded(Err(error.clone()));
                    self.entries.insert(key, entry);
                    self.lru.push_back(key);
                    return Some(Err(error));
                }
                self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                self.evict_until_fit(bytes, cx, window);
                self.used_bytes = self.used_bytes.saturating_add(bytes);
                entry.decoded_bytes = bytes;
            }
            if let Some(result) = result.clone() {
                entry.item = ImageCacheItem::Loaded(result);
            }
            self.entries.insert(key, entry);
            self.lru.push_back(key);
            return result;
        }

        let budget = self.budget_bytes;
        let source = resource.clone();
        let load_future = async move {
            let bytes = match source {
                Resource::Path(path) => {
                    std::fs::read(path.as_ref()).map_err(ImageCacheError::from)?
                }
                _ => {
                    return Err(ImageCacheError::from(anyhow!(
                        "native images require a managed path resource"
                    )));
                }
            };
            Self::decode_bounded(&bytes, budget)
        };
        let task = cx.background_executor().spawn(load_future).shared();
        self.entries.insert(
            key,
            CachedTexture {
                item: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
            },
        );
        self.lru.push_back(key);
        let entity = window.current_view();
        window
            .spawn(cx, async move |cx| {
                _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(entity));
            })
            .detach();
        None
    }
}

#[cfg(target_os = "macos")]
pub fn read_native_pasteboard() -> Option<ClipboardPayload> {
    // Narrow bridge adapted from GPUI 0.2.2 `try_clipboard_image`: AppKit is
    // used only to extract pasteboard payloads, never for editing or layout.
    use cocoa::appkit::{
        NSFilenamesPboardType, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString,
        NSPasteboardTypeTIFF,
    };
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSArray, NSAutoreleasePool, NSData, NSString};
    unsafe fn string_value(value: id) -> Option<String> {
        if value == nil {
            return None;
        }
        let bytes = unsafe { value.UTF8String() as *const u8 };
        if bytes.is_null() {
            return Some(String::new());
        }
        let bytes =
            unsafe { std::ffi::CStr::from_ptr(bytes as *const std::ffi::c_char).to_bytes() };
        std::str::from_utf8(bytes).map(str::to_owned).ok()
    }
    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let image_types = [
            (ImageFormat::Png, NSPasteboardTypePNG),
            (ImageFormat::Tiff, NSPasteboardTypeTIFF),
            (
                ImageFormat::Jpeg,
                NSString::alloc(nil).init_str("public.jpeg").autorelease(),
            ),
            (
                ImageFormat::Gif,
                NSString::alloc(nil)
                    .init_str("com.compuserve.gif")
                    .autorelease(),
            ),
            (
                ImageFormat::Webp,
                NSString::alloc(nil)
                    .init_str("org.webmproject.webp")
                    .autorelease(),
            ),
            (
                ImageFormat::Bmp,
                NSString::alloc(nil)
                    .init_str("com.microsoft.bmp")
                    .autorelease(),
            ),
            (
                ImageFormat::Svg,
                NSString::alloc(nil)
                    .init_str("public.svg-image")
                    .autorelease(),
            ),
        ];
        for (format, ty) in image_types {
            let data = pasteboard.dataForType(ty);
            if data != nil {
                let bytes =
                    std::slice::from_raw_parts(data.bytes() as *const u8, data.length() as usize)
                        .to_vec();
                let text = string_value(pasteboard.stringForType(NSPasteboardTypeString));
                return Some(ClipboardPayload {
                    images: vec![ImagePayload {
                        format,
                        bytes,
                        name: None,
                    }],
                    text,
                    ..Default::default()
                });
            }
        }
        let files = pasteboard.propertyListForType(NSFilenamesPboardType);
        if files != nil {
            let mut file_urls = Vec::new();
            for index in 0..files.count() {
                if let Some(path) = string_value(files.objectAtIndex(index)) {
                    file_urls.push(PathBuf::from(path));
                }
            }
            if !file_urls.is_empty() {
                return Some(ClipboardPayload {
                    file_urls,
                    ..Default::default()
                });
            }
        }
        let text = string_value(pasteboard.stringForType(NSPasteboardTypeString));
        text.map(|text| ClipboardPayload {
            text: Some(text),
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
    use image::{ImageBuffer, Rgba};

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
}
