//! Image intake and bounded decoded-image lifetime for the native editor.
//!
//! The routing types are intentionally independent of the Markdown donor.  The
//! payload extraction follows the donor's image-first classification and the
//! GPUI 0.2.2 `RetainAllImageCache` loading protocol; only the document model
//! decides when a structural `InsertImage` transaction is committed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use base64::Engine as _;
use futures::FutureExt;
use gpui::{
    App, AppContext, ClipboardEntry, ClipboardItem, Entity, Image, ImageCache, ImageCacheError,
    ImageCacheItem, ImageFormat, RenderImage, Resource, WeakEntity, Window, hash,
};
use image::{AnimationDecoder, ImageBuffer, codecs::gif::GifDecoder, imageops::FilterType};
use smallvec::SmallVec;

pub const DECODED_IMAGE_CACHE_BUDGET: usize = 48 * 1024 * 1024;
const MACOS_PROXY_MAX_EDGE: u32 = 1600;
const CONSERVATIVE_PROXY_RESERVATION: usize = 4 * 1024 * 1024;

#[cfg(target_os = "macos")]
mod mac_pressure {
    use std::path::Path;

    const LARGE_RESOURCE_BYTES: u64 = 4 * 1024 * 1024;

    #[cfg(test)]
    use std::sync::{Mutex, OnceLock};

    #[cfg(test)]
    type TestHook = Box<dyn Fn() + Send + Sync + 'static>;

    #[cfg(test)]
    static TEST_HOOK: OnceLock<Mutex<Option<TestHook>>> = OnceLock::new();

    #[link(name = "System")]
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    /// Return unused large malloc-zone pages after a large ImageIO decode has
    /// released its source/thumbnail objects.  This is deliberately called at
    /// the resource-load boundary, never from paint or cache-hit paths.
    pub fn relieve_for_path(path: &Path) {
        let Ok(size) = std::fs::metadata(path).map(|metadata| metadata.len()) else {
            return;
        };
        if size < LARGE_RESOURCE_BYTES {
            return;
        }

        #[cfg(test)]
        if let Ok(guard) = TEST_HOOK.get_or_init(|| Mutex::new(None)).lock()
            && let Some(hook) = guard.as_ref()
        {
            hook();
            return;
        }

        // SAFETY: libSystem accepts a null zone to relieve all malloc zones;
        // this call only asks the allocator to return currently-unused pages.
        unsafe {
            let _ = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }

    #[cfg(test)]
    pub fn set_test_hook(hook: Option<TestHook>) {
        *TEST_HOOK.get_or_init(|| Mutex::new(None)).lock().unwrap() = hook;
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
    File { path: PathBuf, cleanup: bool },
    Text { text: String },
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
    } = payload;
    if let Some(image) = images.into_iter().next() {
        return PasteIntent::Image { payload: image };
    }
    if let Some(path) = file_urls.iter().find(|path| is_supported_image_path(path)) {
        return PasteIntent::File {
            path: path.clone(),
            cleanup: temporary_files.iter().any(|temp| temp == path),
        };
    }
    if let Some(html) = html.as_deref()
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
    rich_text
        .as_ref()
        .or(text.as_ref())
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
    let Some(native) = native else {
        return gpui;
    };
    let Some(gpui) = gpui else {
        return Some(native);
    };
    // A pasteboard can expose a plain-text representation alongside a GPUI
    // image entry. Merge representations before classification so the
    // image-first policy is preserved instead of letting native plain text
    // suppress the image returned by GPUI.
    Some(ClipboardPayload {
        // Prefer an owned native path over GPUI's duplicate byte payload so
        // EXIF dimensions, ImageIO transform, and cleanup share one source.
        images: if native
            .file_urls
            .iter()
            .any(|path| is_supported_image_path(path))
        {
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
    })
}

pub fn classify_drop(paths: &[PathBuf]) -> PasteIntent {
    paths
        .iter()
        .find(|path| is_supported_image_path(path))
        .cloned()
        .map_or(PasteIntent::Unsupported, |path| PasteIntent::File {
            path,
            cleanup: false,
        })
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

fn is_supported_image_path(path: &Path) -> bool {
    path.is_file() && image_format_from_path(path).is_some()
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

    /// Copy an already materialized image into the managed resource directory
    /// without first reading the source into a Rust `Vec`. This is the
    /// production path for Finder drops and pasteboard temporary files.
    pub fn insert_from_path(
        &mut self,
        metadata: ImageMetadata,
        source_path: &Path,
        format: ImageFormat,
    ) -> std::io::Result<u64> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let source = self.resource_root.join(format!(
            "{}.{}",
            metadata.resource_id,
            image_extension(format)
        ));
        std::fs::create_dir_all(&self.resource_root)?;
        if let Err(error) = std::fs::copy(source_path, &source) {
            let _ = std::fs::remove_file(&source);
            return Err(error);
        }
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
}

/// GPUI 0.2.2 image-cache adapter.  It preserves the donor's shared loading
/// task and next-frame notification, while adding exact decoded-byte LRU and
/// `App::drop_image` on eviction.
pub struct BudgetedImageCache {
    budget_bytes: usize,
    used_bytes: usize,
    entries: HashMap<u64, CachedTexture>,
    lru: VecDeque<u64>,
    visible: HashSet<u64>,
    deferred: HashSet<u64>,
    in_flight: usize,
    reserved_bytes: usize,
    reservations: HashMap<u64, (u64, usize)>,
    pending_retries: HashMap<u64, (u64, Resource)>,
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
            entries: HashMap::new(),
            lru: VecDeque::new(),
            visible: HashSet::new(),
            deferred: HashSet::new(),
            in_flight: 0,
            reserved_bytes: 0,
            reservations: HashMap::new(),
            pending_retries: HashMap::new(),
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

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }
    pub fn len(&self) -> usize {
        self.entries.len()
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

    pub(crate) fn decode_resource_bounded(
        resource: &Resource,
        budget_bytes: usize,
    ) -> Result<Arc<RenderImage>, ImageCacheError> {
        let result = match resource {
            Resource::Path(path) => {
                #[cfg(target_os = "macos")]
                {
                    if path.extension().is_some_and(|extension| {
                        extension.to_string_lossy().eq_ignore_ascii_case("svg")
                    }) {
                        let bytes = std::fs::read(path.as_ref()).map_err(ImageCacheError::from)?;
                        Self::decode_bounded(&bytes, budget_bytes)
                    } else {
                        Self::decode_macos_path(path, budget_bytes)
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let bytes = std::fs::read(path.as_ref()).map_err(ImageCacheError::from)?;
                    Self::decode_bounded(&bytes, budget_bytes)
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
            mac_pressure::relieve_for_path(path);
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn decode_macos_path(
        path: &Path,
        budget_bytes: usize,
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
        let max_edge = MACOS_PROXY_MAX_EDGE.min(budget_edge.max(1));
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
                self.used_bytes = self.used_bytes.saturating_sub(entry.decoded_bytes);
                if !self.evict_until_fit(bytes, cx, window) {
                    self.record_drop_image();
                    cx.drop_image(image.clone(), Some(window));
                    // Capacity pressure is a deferred admission, not a
                    // decode failure. Drop the proxy and retry only after a
                    // visible-set or capacity change.
                    self.deferred.insert(key);
                    return None;
                }
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
        if !self.evict_until_fit(reservation_floor, cx, window) {
            self.deferred.insert(key);
            return None;
        }
        let reservation = self
            .budget_bytes
            .saturating_sub(self.used_bytes.saturating_add(self.reserved_bytes));
        if reservation == 0 {
            self.deferred.insert(key);
            return None;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.reserved_bytes = self.reserved_bytes.saturating_add(reservation);
        self.reservations.insert(key, (generation, reservation));
        let budget = reservation;
        let source = resource.clone();
        let load_future = async move { Self::decode_resource_bounded(&source, budget) };
        let task = cx.background_executor().spawn(load_future).shared();
        self.entries.insert(
            key,
            CachedTexture {
                item: ImageCacheItem::Loading(task.clone()),
                decoded_bytes: 0,
                generation,
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
                                cache.used_bytes = cache.used_bytes.saturating_add(decoded_bytes);
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
    html: Option<String>,
    rich_text: Option<String>,
    text: Option<String>,
    image_path: Option<PathBuf>,
    file_urls: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
fn native_payload_from_snapshot(snapshot: NativePasteboardSnapshot) -> Option<ClipboardPayload> {
    let NativePasteboardSnapshot {
        html,
        rich_text,
        text,
        image_path,
        file_urls,
    } = snapshot;
    if let Some(path) = image_path {
        return Some(ClipboardPayload {
            file_urls: vec![path.clone()],
            temporary_files: vec![path],
            html,
            rich_text,
            text,
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
        let _: () = msg_send![allocated, release];
        return None;
    }
    let string: cocoa::base::id = msg_send![attributed, string];
    let result = unsafe { native_string_value(string) };
    let _: () = msg_send![attributed, release];
    result
}

#[cfg(target_os = "macos")]
pub fn read_native_pasteboard() -> Option<ClipboardPayload> {
    // Narrow AppKit bridge: only pasteboard extraction happens here. The
    // editor model, layout, and rendering remain GPUI/native-editor owned.
    use cocoa::appkit::{
        NSFilenamesPboardType, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString,
        NSPasteboardTypeTIFF,
    };
    use cocoa::base::{YES, nil};
    use cocoa::foundation::{NSArray, NSAutoreleasePool, NSData, NSString};

    unsafe {
        let _pool = NSAutoreleasePool::new(nil);
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        let html_type = NSString::alloc(nil).init_str("public.html").autorelease();
        let rtf_type = NSString::alloc(nil).init_str("public.rtf").autorelease();
        let base = NativePasteboardSnapshot {
            html: native_string_value(pasteboard.stringForType(html_type)),
            rich_text: native_rtf_string_value(pasteboard.dataForType(rtf_type)),
            text: native_string_value(pasteboard.stringForType(NSPasteboardTypeString)),
            image_path: None,
            file_urls: Vec::new(),
        };
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
                let path = std::env::temp_dir().join(format!(
                    "joplin-lite-pasteboard-{}.{}",
                    uuid::Uuid::new_v4(),
                    image_extension(format)
                ));
                let path_text = path.to_string_lossy();
                let path_string = NSString::alloc(nil)
                    .init_str(path_text.as_ref())
                    .autorelease();
                let wrote = data.writeToFile_atomically_(path_string, YES);
                if !wrote {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                let mut snapshot = base.clone();
                snapshot.image_path = Some(path);
                return native_payload_from_snapshot(snapshot);
            }
        }
        let files = pasteboard.propertyListForType(NSFilenamesPboardType);
        if files != nil {
            let mut snapshot = base;
            for index in 0..files.count() {
                if let Some(path) = native_string_value(files.objectAtIndex(index)) {
                    snapshot.file_urls.push(PathBuf::from(path));
                }
            }
            return native_payload_from_snapshot(snapshot);
        }
        native_payload_from_snapshot(base)
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
    fn production_clipboard_classification_moves_large_image_bytes() {
        let bytes = vec![0x7f; 16 * 1024 * 1024];
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
