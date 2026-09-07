use crate::note_preview::NotePreview;

use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSBox, NSBoxType, NSCollectionView, NSCollectionViewFlowLayout, NSCollectionViewItem, NSColor,
    NSFont, NSImage, NSImageScaling, NSImageView, NSLineBreakMode, NSTextAlignment, NSTextField,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

pub const NOTE_CARD_IDENTIFIER: &str = "joplin-lite-note-card";

struct NoteCardViewIvars {
    image_view: Retained<NSImageView>,
    title: Retained<NSTextField>,
    snippet: Retained<NSTextField>,
    updated: Retained<NSTextField>,
}

define_class!(
    #[unsafe(super = NSBox)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NoteCardViewIvars]
    struct NoteCardView;
    unsafe impl NSObjectProtocol for NoteCardView {}
);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrowserMetrics {
    pub columns: usize,
    pub card_width: f64,
    pub card_height: f64,
}

pub fn browser_metrics(_available_width: f64) -> BrowserMetrics {
    let card_width = ((_available_width - 8.0) / 2.0).clamp(168.0, 184.0);
    BrowserMetrics {
        columns: 2,
        card_width,
        card_height: card_width * 1.35,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardVisualState {
    pub title: String,
    pub snippet: String,
    pub updated_label: String,
    pub image_id: Option<String>,
}

pub fn card_visual_state(preview: &NotePreview) -> CardVisualState {
    CardVisualState {
        title: preview.title.clone(),
        snippet: preview.snippet.clone(),
        updated_label: preview.updated_label.clone(),
        image_id: preview.first_image_id.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardLayout {
    pub image: (f64, f64, f64, f64),
    pub title: (f64, f64, f64, f64),
    pub snippet: (f64, f64, f64, f64),
    pub updated: (f64, f64, f64, f64),
}

pub fn card_layout(metrics: BrowserMetrics, has_image: bool) -> CardLayout {
    let width = metrics.card_width - 16.0;
    CardLayout {
        image: (8.0, metrics.card_height - 64.0, width, 56.0),
        title: (
            8.0,
            if has_image {
                metrics.card_height - 108.0
            } else {
                metrics.card_height - 60.0
            },
            width,
            40.0,
        ),
        snippet: (
            8.0,
            36.0,
            width,
            if has_image {
                80.0
            } else {
                metrics.card_height - 88.0
            },
        ),
        updated: (8.0, 12.0, width, 18.0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbnailKey {
    pub resource_id: String,
    pub target_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailRequest {
    Start,
    InFlight,
    Negative,
    Deferred,
}

/// Main-thread owned ledger shared by the production request path. It bounds
/// both outstanding work and failed-resource memory, so rapid collection
/// scrolling cannot create an unbounded stream of jobs or failures.
#[derive(Debug, Clone)]
pub struct ThumbnailRequestLedger {
    max_in_flight: usize,
    max_negative: usize,
    in_flight: Vec<ThumbnailKey>,
    negative: Vec<ThumbnailKey>,
}

impl ThumbnailRequestLedger {
    pub fn new(max_in_flight: usize, max_negative: usize) -> Self {
        Self {
            max_in_flight,
            max_negative,
            in_flight: Vec::new(),
            negative: Vec::new(),
        }
    }

    pub fn request(&mut self, key: &ThumbnailKey) -> ThumbnailRequest {
        if self.negative.iter().any(|entry| entry == key) {
            return ThumbnailRequest::Negative;
        }
        if self.in_flight.iter().any(|entry| entry == key) {
            return ThumbnailRequest::InFlight;
        }
        if self.in_flight.len() >= self.max_in_flight {
            return ThumbnailRequest::Deferred;
        }
        self.in_flight.push(key.clone());
        ThumbnailRequest::Start
    }

    pub fn complete(&mut self, key: &ThumbnailKey, success: bool) {
        self.in_flight.retain(|entry| entry != key);
        if !success && self.max_negative != 0 {
            self.negative.retain(|entry| entry != key);
            self.negative.push(key.clone());
            while self.negative.len() > self.max_negative {
                self.negative.remove(0);
            }
        }
    }

    pub fn cancel(&mut self, key: &ThumbnailKey) {
        self.in_flight.retain(|entry| entry != key);
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn negative_len(&self) -> usize {
        self.negative.len()
    }
}

#[derive(Debug, Clone)]
pub struct ThumbnailCache<T> {
    capacity: usize,
    entries: Vec<(ThumbnailKey, T)>,
}

impl<T> ThumbnailCache<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::new(),
        }
    }

    pub fn get(&mut self, key: &ThumbnailKey) -> Option<T>
    where
        T: Clone,
    {
        let index = self.entries.iter().position(|(entry, _)| entry == key)?;
        let (entry, value) = self.entries.remove(index);
        let clone = value.clone();
        self.entries.push((entry, value));
        Some(clone)
    }

    pub fn insert(&mut self, key: ThumbnailKey, value: T) {
        if self.capacity == 0 {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(entry, _)| entry == &key) {
            self.entries.remove(index);
        }
        self.entries.push((key, value));
        while self.entries.len() > self.capacity {
            self.entries.remove(0);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

pub fn selected_index_for_id(ids: &[String], selected_id: Option<&str>) -> Option<usize> {
    selected_id.and_then(|id| ids.iter().position(|candidate| candidate == id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewListUpdate {
    ReloadAll,
    ReloadIndices(Vec<usize>),
}

pub fn preview_list_update(old: &[NotePreview], new: &[NotePreview]) -> PreviewListUpdate {
    let old_ids = old
        .iter()
        .map(|preview| &preview.note_id)
        .collect::<Vec<_>>();
    let new_ids = new
        .iter()
        .map(|preview| &preview.note_id)
        .collect::<Vec<_>>();
    if old_ids != new_ids {
        return PreviewListUpdate::ReloadAll;
    }
    PreviewListUpdate::ReloadIndices(
        old.iter()
            .zip(new)
            .enumerate()
            .filter_map(|(index, (before, after))| (before != after).then_some(index))
            .collect(),
    )
}

pub fn restore_selection_after_failed_switch(
    ids: &[String],
    previous_id: Option<&str>,
) -> Option<usize> {
    selected_index_for_id(ids, previous_id)
}

pub fn thumbnail_display_size(
    pixels: (usize, usize),
    target_pixel_size: usize,
) -> Option<(f64, f64)> {
    if pixels.0 == 0 || pixels.1 == 0 || target_pixel_size == 0 {
        return None;
    }
    let scale = 56.0 / target_pixel_size as f64;
    Some((pixels.0 as f64 * scale, pixels.1 as f64 * scale))
}

pub fn make_note_collection_view(
    mtm: MainThreadMarker,
    frame: NSRect,
) -> Retained<NSCollectionView> {
    let collection = NSCollectionView::initWithFrame(NSCollectionView::alloc(mtm), frame);
    let metrics = browser_metrics(frame.size.width);
    let layout = NSCollectionViewFlowLayout::new(mtm);
    layout.setItemSize(NSSize::new(metrics.card_width, metrics.card_height));
    layout.setMinimumInteritemSpacing(8.0);
    layout.setMinimumLineSpacing(8.0);
    layout.setSectionInset(objc2_foundation::NSEdgeInsets {
        top: 8.0,
        left: 0.0,
        bottom: 16.0,
        right: 0.0,
    });
    collection.setCollectionViewLayout(Some(&layout));
    collection.setSelectable(true);
    collection.setAllowsMultipleSelection(false);
    collection.setAllowsEmptySelection(true);
    let identifier = NSString::from_str(NOTE_CARD_IDENTIFIER);
    unsafe {
        collection
            .registerClass_forItemWithIdentifier(Some(NSCollectionViewItem::class()), &identifier);
    }
    collection
}

pub fn configure_note_card(
    item: &NSCollectionViewItem,
    preview: &NotePreview,
    image: Option<&NSImage>,
    selected: bool,
    metrics: BrowserMetrics,
    mtm: MainThreadMarker,
) {
    let card = match item.view().downcast::<NoteCardView>() {
        Ok(card) => card,
        Err(_) => {
            let card = NoteCardView::new(mtm, metrics);
            item.setView(&card);
            card
        }
    };
    card.configure(preview, image, selected, metrics);
}

impl NoteCardView {
    fn new(mtm: MainThreadMarker, metrics: BrowserMetrics) -> Retained<Self> {
        let frames = card_layout(metrics, false);
        let image_view =
            NSImageView::initWithFrame(NSImageView::alloc(mtm), frame_from_tuple(frames.image));
        image_view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        image_view.setHidden(true);
        let title = card_label(
            mtm,
            "",
            frame_from_tuple(frames.title),
            13.0,
            NSColor::labelColor(),
            2,
        );
        let snippet = card_label(
            mtm,
            "",
            frame_from_tuple(frames.snippet),
            11.0,
            NSColor::secondaryLabelColor(),
            2,
        );
        let updated = card_label(
            mtm,
            "",
            frame_from_tuple(frames.updated),
            10.0,
            NSColor::tertiaryLabelColor(),
            1,
        );
        let this = Self::alloc(mtm).set_ivars(NoteCardViewIvars {
            image_view,
            title,
            snippet,
            updated,
        });
        let card: Retained<Self> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(metrics.card_width, metrics.card_height),
            )]
        };
        card.setBoxType(NSBoxType::Custom);
        card.setTransparent(false);
        card.setCornerRadius(8.0);
        card.setBorderWidth(1.0);
        card.setFillColor(&NSColor::textBackgroundColor());
        card.addSubview(&card.ivars().image_view);
        card.addSubview(&card.ivars().title);
        card.addSubview(&card.ivars().snippet);
        card.addSubview(&card.ivars().updated);
        card
    }

    fn configure(
        &self,
        preview: &NotePreview,
        image: Option<&NSImage>,
        selected: bool,
        metrics: BrowserMetrics,
    ) {
        // NSCollectionViewFlowLayout owns this root frame. The data-source
        // callback runs before layout attributes are applied; changing it here
        // clobbers the column origin of a reused item.
        let border_color = if selected {
            evernote_green()
        } else {
            NSColor::separatorColor()
        };
        self.setBorderColor(&border_color);

        let frames = card_layout(metrics, image.is_some());
        self.ivars()
            .image_view
            .setFrame(frame_from_tuple(frames.image));
        self.ivars().image_view.setImage(image);
        self.ivars().image_view.setHidden(image.is_none());
        self.ivars()
            .image_view
            .setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        self.ivars().title.setFrame(frame_from_tuple(frames.title));
        self.ivars()
            .snippet
            .setFrame(frame_from_tuple(frames.snippet));
        self.ivars()
            .updated
            .setFrame(frame_from_tuple(frames.updated));
        self.ivars()
            .title
            .setStringValue(&NSString::from_str(&preview.title));
        self.ivars()
            .snippet
            .setStringValue(&NSString::from_str(&preview.snippet));
        self.ivars()
            .updated
            .setStringValue(&NSString::from_str(&preview.updated_label));
    }
}

fn evernote_green() -> Retained<NSColor> {
    NSColor::colorWithCalibratedRed_green_blue_alpha(0.29, 0.65, 0.37, 1.0)
}

fn frame_from_tuple(frame: (f64, f64, f64, f64)) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.0, frame.1),
        NSSize::new(frame.2, frame.3),
    )
}

fn card_label(
    mtm: MainThreadMarker,
    value: &str,
    frame: NSRect,
    font_size: f64,
    color: Retained<NSColor>,
    lines: isize,
) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setStringValue(&NSString::from_str(value));
    field.setBezeled(false);
    field.setDrawsBackground(false);
    field.setEditable(false);
    field.setSelectable(false);
    field.setFont(Some(&NSFont::systemFontOfSize(font_size)));
    field.setTextColor(Some(&color));
    field.setAlignment(NSTextAlignment::Left);
    field.setUsesSingleLineMode(false);
    field.setMaximumNumberOfLines(lines);
    field.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    field
}

#[cfg(test)]
mod tests {
    use super::{
        CardVisualState, PreviewListUpdate, ThumbnailCache, ThumbnailKey, ThumbnailRequest,
        ThumbnailRequestLedger, browser_metrics, card_layout, card_visual_state,
        preview_list_update, restore_selection_after_failed_switch, selected_index_for_id,
        thumbnail_display_size,
    };
    use crate::note_preview::NotePreview;

    #[test]
    fn red_browser_metrics_keep_two_cards_inside_the_360_to_400_rail() {
        for width in [360.0, 400.0] {
            let metrics = browser_metrics(width);
            assert_eq!(metrics.columns, 2);
            assert!((168.0..=184.0).contains(&metrics.card_width));
            assert_eq!(metrics.card_height, metrics.card_width * 1.35);
            assert!(metrics.card_width * 2.0 <= width);
        }
    }

    #[test]
    fn red_card_reuse_clears_an_old_image_and_replaces_all_content() {
        let with_image = NotePreview {
            note_id: "a".into(),
            title: "A".into(),
            snippet: "old".into(),
            updated_label: "刚刚".into(),
            first_image_id: Some("image-a".into()),
            updated_time: 1,
        };
        let without_image = NotePreview {
            note_id: "b".into(),
            title: "B".into(),
            snippet: "new".into(),
            updated_label: "昨天".into(),
            first_image_id: None,
            updated_time: 2,
        };
        assert_eq!(
            card_visual_state(&with_image),
            CardVisualState {
                title: "A".into(),
                snippet: "old".into(),
                updated_label: "刚刚".into(),
                image_id: Some("image-a".into()),
            }
        );
        assert_eq!(
            card_visual_state(&without_image),
            CardVisualState {
                title: "B".into(),
                snippet: "new".into(),
                updated_label: "昨天".into(),
                image_id: None,
            }
        );
    }

    #[test]
    fn red_card_content_layout_reclaims_image_space_when_reused_without_image() {
        let metrics = browser_metrics(360.0);
        let with_image = card_layout(metrics, true);
        let without_image = card_layout(metrics, false);
        assert!(without_image.title.1 > with_image.title.1);
        assert!(without_image.snippet.3 > with_image.snippet.3);
        assert_eq!(without_image.image.2, with_image.image.2);
    }

    #[test]
    fn red_thumbnail_cache_is_bounded_and_keyed_by_resource_and_size() {
        let mut cache = ThumbnailCache::new(2);
        let a = ThumbnailKey {
            resource_id: "a".into(),
            target_size: 112,
        };
        let b = ThumbnailKey {
            resource_id: "b".into(),
            target_size: 112,
        };
        let c = ThumbnailKey {
            resource_id: "c".into(),
            target_size: 112,
        };
        cache.insert(a.clone(), 1);
        cache.insert(b.clone(), 2);
        assert_eq!(cache.get(&a), Some(1));
        cache.insert(c.clone(), 3);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&b), None);
        assert_eq!(cache.get(&a), Some(1));
        assert_eq!(cache.get(&c), Some(3));
    }

    #[test]
    fn thumbnail_requests_deduplicate_and_negative_cache_is_bounded() {
        let mut ledger = ThumbnailRequestLedger::new(2, 2);
        let a = ThumbnailKey {
            resource_id: "a".into(),
            target_size: 112,
        };
        let b = ThumbnailKey {
            resource_id: "b".into(),
            target_size: 112,
        };
        let c = ThumbnailKey {
            resource_id: "c".into(),
            target_size: 112,
        };
        assert_eq!(ledger.request(&a), ThumbnailRequest::Start);
        assert_eq!(ledger.request(&a), ThumbnailRequest::InFlight);
        assert_eq!(ledger.request(&b), ThumbnailRequest::Start);
        assert_eq!(ledger.request(&c), ThumbnailRequest::Deferred);
        ledger.complete(&a, false);
        assert_eq!(ledger.request(&a), ThumbnailRequest::Negative);
        assert_eq!(ledger.request(&c), ThumbnailRequest::Start);
        ledger.complete(&b, false);
        assert_eq!(ledger.negative_len(), 2);
        ledger.complete(&a, false);
        ledger.complete(&c, false);
        assert_eq!(ledger.in_flight_len(), 0);
    }

    #[test]
    fn thumbnail_display_size_preserves_non_square_ratio_and_target_scale() {
        let size = thumbnail_display_size((112, 63), 112).unwrap();
        assert_eq!(size, (56.0, 31.5));
        assert!((size.0 / size.1 - 112.0 / 63.0).abs() < f64::EPSILON);
        assert!(thumbnail_display_size((0, 63), 112).is_none());
    }

    #[test]
    fn red_selection_id_survives_filter_and_reorder() {
        let selected = Some("note-b");
        assert_eq!(
            selected_index_for_id(&["note-a".into(), "note-b".into()], selected),
            Some(1)
        );
        assert_eq!(
            selected_index_for_id(&["note-b".into(), "note-a".into()], selected),
            Some(0)
        );
        assert_eq!(selected_index_for_id(&["note-a".into()], selected), None);
    }

    #[test]
    fn failed_switch_restores_previous_selection_by_id() {
        let ids = vec!["old".into(), "new".into()];
        assert_eq!(
            restore_selection_after_failed_switch(&ids, Some("old")),
            Some(0)
        );
        assert_eq!(
            restore_selection_after_failed_switch(&ids, Some("gone")),
            None
        );
    }

    #[test]
    fn same_order_only_reloads_changed_cards_but_reorder_reloads_collection() {
        let old = vec![NotePreview {
            note_id: "a".into(),
            title: "A".into(),
            snippet: "".into(),
            updated_label: "刚刚".into(),
            first_image_id: None,
            updated_time: 1,
        }];
        let mut changed = old.clone();
        changed[0].snippet = "changed".into();
        assert_eq!(
            preview_list_update(&old, &changed),
            PreviewListUpdate::ReloadIndices(vec![0])
        );
        let reordered = vec![NotePreview {
            note_id: "b".into(),
            ..old[0].clone()
        }];
        assert_eq!(
            preview_list_update(&old, &reordered),
            PreviewListUpdate::ReloadAll
        );
    }
}
