//! Bounded, list-only thumbnail sources for the Cards presentation.
//!
//! A note-list projection contains a thumbnail *identity*, not resource bytes.
//! This manager keeps that boundary intact: a visible card is materialized from
//! a descriptor-safe repository reader on a background task, then the card
//! cache decodes its small proxy independently from the active editor session.
//! The task-owned lease is deliberately not an [`ImageStore`] root.  If the
//! library window disappears before the worker finishes, dropping the result
//! removes its private staging directory rather than recreating an old editor
//! cache path.

use crate::native_editor::images::{ImageMetadata, ImageStore, inspect_persisted_image};
use app_lite_core::{LibraryRepository, ResourceId};
use gpui::Resource;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
use futures::channel::oneshot;

/// The card image cache never needs editor-sized textures.  A 192pt proxy at
/// normal scale is enough for the 76pt card thumbnail while retaining a small
/// amount of room for a short visible list.
pub(crate) const CARD_THUMBNAIL_PROXY_EDGE: u32 = 192;
pub(crate) const CARD_THUMBNAIL_CACHE_BUDGET: usize = 4 * 1024 * 1024;
/// The only persisted card source is a 192px PNG proxy.  Twenty MiB therefore
/// holds at least forty worst-case 512KiB proxies, enough for a normal 6–8 card
/// viewport while remaining strictly bounded across a long scrolling session.
pub(crate) const CARD_THUMBNAIL_SOURCE_CACHE_BUDGET: usize = 2 * app_lite_core::MAX_IMAGE_BYTES;
pub(crate) const CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES: usize = 512 * 1024;

/// One worker-owned source directory.  The parent temporary directory is not
/// itself retained as a cache root; only the randomized child belongs to this
/// job and can therefore be removed without touching any editor session.
pub(crate) struct CardThumbnailLease {
    root: PathBuf,
    source: PathBuf,
    source_bytes: usize,
}

#[cfg(test)]
struct BackgroundCardThumbnailGate {
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(test)]
impl BackgroundCardThumbnailGate {
    async fn wait(&self) {
        let receiver = self.release.lock().expect("thumbnail gate poisoned").take();
        if let Some(receiver) = receiver {
            let _ = receiver.await;
        }
    }
}

impl CardThumbnailLease {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join("joplin-lite-card-thumbnail-staging")
            .join(uuid::Uuid::new_v4().simple().to_string());
        Self {
            source: root.join("unmaterialized"),
            root,
            source_bytes: 0,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn record_source(&mut self, source: PathBuf) -> std::io::Result<()> {
        self.source_bytes = usize::try_from(source.metadata()?.len())
            .map_err(|_| std::io::Error::other("card thumbnail source size does not fit usize"))?;
        self.source = source;
        Ok(())
    }

    fn source(&self) -> &Path {
        &self.source
    }

    fn source_bytes(&self) -> usize {
        self.source_bytes
    }
}

impl Drop for CardThumbnailLease {
    fn drop(&mut self) {
        // The root is a unique child generated above.  The resource store is
        // authoritative, so cleanup is best effort and can never sweep its
        // shared parent.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(crate) struct CardThumbnailJob {
    resource_id: ResourceId,
    repository: Arc<LibraryRepository>,
    #[cfg(test)]
    gate: Option<Arc<BackgroundCardThumbnailGate>>,
}

pub(crate) enum CardThumbnailCompletion {
    Ready {
        resource_id: ResourceId,
        lease: CardThumbnailLease,
    },
    Failed {
        resource_id: ResourceId,
    },
}

/// A coalescing visible-set manager.  At most one descriptor-safe worker runs
/// at a time, and the queue is reconstructed from the current uniform-list
/// range so a rapid scroll cannot turn an offscreen history into a backlog.
pub(crate) struct CardThumbnailManager {
    repository: Arc<LibraryRepository>,
    desired: HashSet<ResourceId>,
    queued: VecDeque<ResourceId>,
    pending: Option<ResourceId>,
    ready: HashMap<ResourceId, CardThumbnailLease>,
    source_lru: VecDeque<ResourceId>,
    source_bytes: usize,
    deferred: HashSet<ResourceId>,
    failed: HashSet<ResourceId>,
    #[cfg(test)]
    next_materialization_gate: Option<Arc<BackgroundCardThumbnailGate>>,
}

impl CardThumbnailManager {
    pub(crate) fn new(repository: Arc<LibraryRepository>) -> Self {
        Self {
            repository,
            desired: HashSet::new(),
            queued: VecDeque::new(),
            pending: None,
            ready: HashMap::new(),
            source_lru: VecDeque::new(),
            source_bytes: 0,
            deferred: HashSet::new(),
            failed: HashSet::new(),
            #[cfg(test)]
            next_materialization_gate: None,
        }
    }

    /// Replace, rather than append to, the target set. `uniform_list` calls
    /// this from its bounded construction range, which is the only place list
    /// viewport residency is known. A nonempty change retains small proxy
    /// leases in a strict LRU. An empty target may simply mean the current
    /// Cards viewport has text-only notes, so it removes decoded residency
    /// without discarding the bounded source LRU; [`Self::leave_cards`] owns
    /// the real mode/visibility teardown.
    pub(crate) fn reconcile_desired(&mut self, resource_ids: impl IntoIterator<Item = ResourceId>) {
        let desired = resource_ids.into_iter().collect::<HashSet<_>>();
        let changed = desired != self.desired;
        if desired.is_empty() {
            self.desired = desired;
            self.queued.clear();
            self.failed.clear();
            self.deferred.clear();
            return;
        }
        if changed {
            // Capacity deferral is not a corruption state. A real viewport
            // change can free an offscreen proxy, so it earns one new attempt.
            self.deferred.clear();
        }
        self.desired = desired;
        self.failed
            .retain(|resource_id| self.desired.contains(resource_id));
        self.deferred
            .retain(|resource_id| self.desired.contains(resource_id));
        let retained = self
            .desired
            .iter()
            .filter(|resource_id| self.ready.contains_key(*resource_id))
            .cloned()
            .collect::<Vec<_>>();
        for resource_id in retained {
            self.touch_ready(&resource_id);
        }
        self.queued.retain(|resource_id| {
            self.desired.contains(resource_id)
                && !self.ready.contains_key(resource_id)
                && self.pending.as_ref() != Some(resource_id)
                && !self.deferred.contains(resource_id)
                && !self.failed.contains(resource_id)
        });
        for resource_id in &self.desired {
            if !self.ready.contains_key(resource_id)
                && self.pending.as_ref() != Some(resource_id)
                && !self.deferred.contains(resource_id)
                && !self.failed.contains(resource_id)
                && !self.queued.contains(resource_id)
            {
                self.queued.push_back(resource_id.clone());
            }
        }
    }

    /// Release card-only source leases when Cards itself is no longer
    /// rendered. A pending worker is intentionally left tracked until it
    /// finishes: clearing it would allow a quick Cards re-entry to launch a
    /// duplicate descriptor verification while the original worker is still
    /// reading the same durable blob.
    pub(crate) fn leave_cards(&mut self) {
        self.desired.clear();
        self.queued.clear();
        self.failed.clear();
        self.deferred.clear();
        self.clear_ready_sources();
    }

    pub(crate) fn next_job(&mut self) -> Option<CardThumbnailJob> {
        while let Some(resource_id) = self.queued.pop_front() {
            if !self.desired.contains(&resource_id)
                || self.ready.contains_key(&resource_id)
                || self.deferred.contains(&resource_id)
                || self.failed.contains(&resource_id)
            {
                continue;
            }
            self.pending = Some(resource_id.clone());
            return Some(CardThumbnailJob {
                resource_id,
                repository: Arc::clone(&self.repository),
                #[cfg(test)]
                gate: self.next_materialization_gate.take(),
            });
        }
        None
    }

    /// Runs entirely off the GPUI foreground thread. One descriptor-safe open
    /// validates the durable blob, then its reset reader is copied only to an
    /// ephemeral task file so ImageIO can build a bounded 192px PNG proxy.
    /// The original never becomes a retained card lease.
    pub(crate) async fn materialize(job: CardThumbnailJob) -> CardThumbnailCompletion {
        #[cfg(test)]
        if let Some(gate) = job.gate.as_ref() {
            gate.wait().await;
        }
        let resource_id = job.resource_id;
        let result = (|| {
            let Some((metadata, mut source_reader)) = job
                .repository
                .open_verified_resource_file(&resource_id)
                .map_err(|_| ())?
            else {
                return Err(());
            };
            let inspection = source_reader.try_clone().map_err(|_| ())?;
            let (format, natural_size) =
                inspect_persisted_image(inspection, &metadata.mime).map_err(|_| ())?;
            // Unix descriptor clones share an offset. The repository has
            // already verified this descriptor; reset it rather than paying a
            // second full SHA-256 pass before the bounded proxy materializes.
            source_reader.seek(SeekFrom::Start(0)).map_err(|_| ())?;
            let mut lease = CardThumbnailLease::new();
            let source = ImageStore::materialize_bounded_thumbnail_proxy_from_verified_reader(
                lease.root(),
                &ImageMetadata::new(resource_id.as_str(), natural_size.0, natural_size.1),
                source_reader,
                format,
                CARD_THUMBNAIL_PROXY_EDGE,
                CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES,
            )
            .map_err(|_| ())?;
            lease.record_source(source).map_err(|_| ())?;
            Ok(lease)
        })();
        match result {
            Ok(lease) => CardThumbnailCompletion::Ready { resource_id, lease },
            Err(()) => CardThumbnailCompletion::Failed { resource_id },
        }
    }

    pub(crate) fn finish(&mut self, completion: CardThumbnailCompletion) {
        match completion {
            CardThumbnailCompletion::Ready { resource_id, lease } => {
                if self.pending.as_ref() == Some(&resource_id) {
                    self.pending = None;
                }
                if self.desired.contains(&resource_id) {
                    if !self.adopt_ready(resource_id.clone(), lease) {
                        // The proxy budget is full of visible resources. Do
                        // not evict a currently visible thumbnail or launch a
                        // retry loop; the next true viewport change retries.
                        self.deferred.insert(resource_id);
                    }
                }
                // Otherwise the task-owned lease drops here, before any
                // offscreen source can become visible.
            }
            CardThumbnailCompletion::Failed { resource_id } => {
                if self.pending.as_ref() == Some(&resource_id) {
                    self.pending = None;
                }
                if self.desired.contains(&resource_id) {
                    // A render retry must not hammer a bad file every frame.
                    // Leaving and re-entering the bounded visible set gives
                    // the repository a later recovery opportunity.
                    self.failed.insert(resource_id);
                }
            }
        }
    }

    pub(crate) fn source_for(&self, resource_id: Option<&ResourceId>) -> Option<PathBuf> {
        resource_id.and_then(|resource_id| {
            self.ready
                .get(resource_id)
                .map(|lease| lease.source().to_path_buf())
        })
    }

    pub(crate) fn visible_resources(&self) -> Vec<Resource> {
        self.ready
            .iter()
            .filter(|(resource_id, _)| self.desired.contains(*resource_id))
            .map(|(_, lease)| Resource::from(lease.source().to_path_buf()))
            .collect()
    }

    pub(crate) fn failed(&self, resource_id: Option<&ResourceId>) -> bool {
        resource_id.is_some_and(|resource_id| self.failed.contains(resource_id))
    }

    fn clear_ready_sources(&mut self) {
        self.ready.clear();
        self.source_lru.clear();
        self.source_bytes = 0;
    }

    fn touch_ready(&mut self, resource_id: &ResourceId) {
        self.source_lru.retain(|candidate| candidate != resource_id);
        if self.ready.contains_key(resource_id) {
            self.source_lru.push_back(resource_id.clone());
        }
    }

    fn remove_ready(&mut self, resource_id: &ResourceId) -> Option<CardThumbnailLease> {
        self.source_lru.retain(|candidate| candidate != resource_id);
        let lease = self.ready.remove(resource_id)?;
        self.source_bytes = self.source_bytes.saturating_sub(lease.source_bytes());
        Some(lease)
    }

    fn evict_offscreen_until_fits(&mut self, required: usize) -> bool {
        while self.source_bytes.saturating_add(required) > CARD_THUMBNAIL_SOURCE_CACHE_BUDGET {
            let Some(index) = self
                .source_lru
                .iter()
                .position(|resource_id| !self.desired.contains(resource_id))
            else {
                return false;
            };
            let Some(resource_id) = self.source_lru.get(index).cloned() else {
                return false;
            };
            let _ = self.remove_ready(&resource_id);
        }
        true
    }

    fn adopt_ready(&mut self, resource_id: ResourceId, lease: CardThumbnailLease) -> bool {
        let required = lease.source_bytes();
        if required > CARD_THUMBNAIL_SOURCE_CACHE_BUDGET
            || !self.evict_offscreen_until_fits(required)
        {
            return false;
        }
        let _ = self.remove_ready(&resource_id);
        self.source_bytes = self.source_bytes.saturating_add(required);
        self.ready.insert(resource_id.clone(), lease);
        self.touch_ready(&resource_id);
        true
    }

    #[cfg(test)]
    pub(crate) fn stall_next_materialization_for_test(&mut self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        self.next_materialization_gate = Some(Arc::new(BackgroundCardThumbnailGate {
            release: Mutex::new(Some(receiver)),
        }));
        sender
    }

    #[cfg(test)]
    pub(crate) fn ready_count_for_test(&self) -> usize {
        self.ready.len()
    }

    #[cfg(test)]
    pub(crate) fn desired_count_for_test(&self) -> usize {
        self.desired.len()
    }

    #[cfg(test)]
    pub(crate) fn desired_contains_for_test(&self, resource_id: &ResourceId) -> bool {
        self.desired.contains(resource_id)
    }

    #[cfg(test)]
    pub(crate) fn queued_count_for_test(&self) -> usize {
        self.queued.len()
    }

    #[cfg(test)]
    pub(crate) fn source_bytes_for_test(&self) -> usize {
        self.source_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn resource_id(index: u8) -> ResourceId {
        ResourceId::new(format!("{index:032x}")).expect("fixture resource id")
    }

    fn large_jpeg(tint: u8) -> Vec<u8> {
        // The source must be materially larger than the card proxy budget. A
        // high-entropy pattern prevents the JPEG encoder from reducing this to
        // a trivial flat-color fixture.
        let image = ImageBuffer::from_fn(1_600, 1_200, |x, y| {
            let mix = (x as u8)
                .wrapping_mul(31)
                .wrapping_add((y as u8).wrapping_mul(17))
                .wrapping_add(tint);
            Rgba([mix, mix.rotate_left(3), mix.rotate_left(5), 0xff])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .expect("encode large card fixture");
        let bytes = encoded.into_inner();
        assert!(
            bytes.len() > 512 * 1024,
            "the fixture must distinguish a full original from a bounded card proxy"
        );
        bytes
    }

    fn store_large_jpeg(repository: &LibraryRepository, tint: u8) -> ResourceId {
        repository
            .import_resource(
                &large_jpeg(tint),
                &format!("large-card-{tint}.jpeg"),
                "image/jpeg",
                "jpeg",
            )
            .expect("store card image resource")
    }

    fn proxy_lease(bytes: usize) -> CardThumbnailLease {
        let mut lease = CardThumbnailLease::new();
        std::fs::create_dir_all(lease.root()).expect("create private proxy lease root");
        let source = lease.root().join("proxy.png");
        std::fs::write(&source, vec![0x5a; bytes]).expect("write bounded proxy fixture");
        lease
            .record_source(source)
            .expect("record bounded proxy fixture");
        lease
    }

    #[test]
    fn leaving_cards_drops_every_private_thumbnail_proxy_lease() {
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let old = resource_id(1);
        let mut manager = CardThumbnailManager::new(repository);
        let mut lease = CardThumbnailLease::new();
        std::fs::create_dir_all(lease.root()).expect("create private test lease root");
        let source = lease.root().join("old.jpg");
        std::fs::write(&source, b"thumbnail fixture").expect("write private lease source");
        lease
            .record_source(source.clone())
            .expect("record private lease source");
        manager.ready.insert(old.clone(), lease);
        assert!(source.is_file());

        manager.leave_cards();

        assert!(manager.source_for(Some(&old)).is_none());
        assert!(
            !source.exists(),
            "leaving Cards must release all task-owned proxy files"
        );
    }

    #[test]
    fn card_materialization_writes_a_bounded_proxy_instead_of_the_large_original() {
        // Mutation-sensitive: restoring ImageStore::materialize_durable_reader_at
        // here writes the original JPEG to the card lease, so this assertion
        // immediately catches a full-size cache source even though the decoded
        // GPUI texture itself remains small.
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let resource_id = store_large_jpeg(&repository, 0x41);
        let mut manager = CardThumbnailManager::new(repository);
        manager.reconcile_desired([resource_id.clone()]);
        let completion = futures::executor::block_on(CardThumbnailManager::materialize(
            manager
                .next_job()
                .expect("visible card should schedule a worker"),
        ));
        manager.finish(completion);

        let source = manager
            .source_for(Some(&resource_id))
            .expect("successful card materialization should expose its proxy");
        assert!(
            source.metadata().expect("proxy metadata").len() <= 512 * 1024,
            "the card lease must retain only a bounded proxy, never the original JPEG"
        );
    }

    #[test]
    fn card_proxy_lru_reuses_a_after_b_without_a_second_verified_open() {
        // Mutation-sensitive: immediate `ready.retain(desired)` eviction makes
        // the final A request schedule a new worker, re-hash the durable blob,
        // and recopy it. The production card path must instead retain a
        // bounded proxy LRU across the short A -> B -> A viewport cycle.
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let first = store_large_jpeg(&repository, 0x21);
        let second = store_large_jpeg(&repository, 0x92);
        let verified_opens = repository.observe_verified_resource_opens();
        let mut manager = CardThumbnailManager::new(Arc::clone(&repository));

        manager.reconcile_desired([first.clone()]);
        let first_job = manager
            .next_job()
            .expect("first card should schedule a worker");
        manager.finish(futures::executor::block_on(
            CardThumbnailManager::materialize(first_job),
        ));
        let first_proxy = manager
            .source_for(Some(&first))
            .expect("first card proxy should be retained");

        manager.reconcile_desired([second.clone()]);
        let second_job = manager
            .next_job()
            .expect("second card should schedule a worker");
        manager.finish(futures::executor::block_on(
            CardThumbnailManager::materialize(second_job),
        ));

        manager.reconcile_desired([first.clone()]);
        assert_eq!(
            manager.source_for(Some(&first)),
            Some(first_proxy),
            "returning to A inside the bounded source cache must reuse its proxy"
        );
        assert!(
            manager.next_job().is_none(),
            "a retained A proxy must not schedule another materialization"
        );
        assert_eq!(
            verified_opens.try_iter().count(),
            2,
            "A and B each need one descriptor-safe verification, not an A re-hash or an inspect/copy double-open"
        );
    }

    #[test]
    fn text_only_cards_viewport_keeps_bounded_proxy_lru_until_cards_exit() {
        // Mutation-sensitive: an empty desired set can mean that the current
        // Cards viewport contains only text notes; it does *not* mean Cards
        // was dismissed. Clearing `ready` here turns A -> text -> A into a
        // second full verified open/copy even though the bounded source LRU
        // still has ample capacity.
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let thumbnail = store_large_jpeg(&repository, 0x58);
        let verified_opens = repository.observe_verified_resource_opens();
        let mut manager = CardThumbnailManager::new(Arc::clone(&repository));

        manager.reconcile_desired([thumbnail.clone()]);
        let job = manager
            .next_job()
            .expect("visible image card should schedule one worker");
        let completion = futures::executor::block_on(CardThumbnailManager::materialize(job));
        manager.finish(completion);
        let proxy = manager
            .source_for(Some(&thumbnail))
            .expect("visible image card should retain a proxy");

        manager.reconcile_desired(std::iter::empty());
        assert_eq!(
            manager.source_for(Some(&thumbnail)),
            Some(proxy.clone()),
            "a text-only Cards viewport must evict its decoded target, not discard the bounded source lease"
        );

        manager.reconcile_desired([thumbnail.clone()]);
        assert_eq!(
            manager.source_for(Some(&thumbnail)),
            Some(proxy),
            "returning from a text-only Cards viewport must reuse the retained proxy"
        );
        assert!(
            manager.next_job().is_none(),
            "a retained source must not schedule a second full verification"
        );
        assert_eq!(
            verified_opens.try_iter().count(),
            1,
            "A -> text-only Cards -> A must verify the durable source exactly once"
        );
    }

    #[test]
    fn card_proxy_cache_keeps_an_eight_card_viewport_without_source_deferral() {
        // Mutation-sensitive: evicting visible entries merely to satisfy the
        // source budget makes an ordinary 6–8 card viewport intermittently
        // blank. A 20MiB cache must admit this hand-checked proxy set intact.
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let resource_ids = (0..8).map(resource_id).collect::<Vec<_>>();
        let mut manager = CardThumbnailManager::new(repository);
        manager.reconcile_desired(resource_ids.clone());
        for resource_id in &resource_ids {
            manager.finish(CardThumbnailCompletion::Ready {
                resource_id: resource_id.clone(),
                lease: proxy_lease(CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES),
            });
        }

        assert_eq!(
            manager.ready_count_for_test(),
            8,
            "a normal Cards viewport must keep every visible proxy source"
        );
        assert!(
            manager.source_bytes_for_test() <= CARD_THUMBNAIL_SOURCE_CACHE_BUDGET,
            "visible card sources must remain within the explicit disk budget"
        );
    }

    #[test]
    fn card_proxy_cache_never_exceeds_its_disk_budget_under_visible_pressure() {
        // Mutation-sensitive: deleting the `evict_offscreen_until_fits` guard
        // lets a long-lived, all-visible fixture retain more than 20MiB of
        // task-private disk sources. The final source may defer, but stored
        // proxies must never cross the hard budget.
        let profile = tempfile::tempdir().expect("temporary thumbnail profile");
        let repository = Arc::new(
            LibraryRepository::open(profile.path().join("library.sqlite"))
                .expect("open thumbnail repository"),
        );
        let resource_ids = (0..41).map(resource_id).collect::<Vec<_>>();
        let mut manager = CardThumbnailManager::new(repository);
        manager.reconcile_desired(resource_ids.clone());
        for resource_id in resource_ids {
            manager.finish(CardThumbnailCompletion::Ready {
                resource_id,
                lease: proxy_lease(CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES),
            });
        }

        assert_eq!(
            manager.source_bytes_for_test(),
            CARD_THUMBNAIL_SOURCE_CACHE_BUDGET,
            "the 41st 512KiB proxy must not overflow a 20MiB source cache"
        );
        assert_eq!(
            manager.ready_count_for_test(),
            40,
            "capacity pressure may defer one source, but never evict a visible proxy"
        );
    }
}
