use super::*;
use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryShellState, NoteId, ResourceId};
use gpui::{ImageCache, Resource, TestAppContext, VisualTestContext};
use image::{ImageBuffer, Rgba};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::TryRecvError;

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn repository() -> (tempfile::TempDir, Arc<LibraryRepository>) {
    let profile = tempfile::tempdir().expect("temporary profile");
    let repository = Arc::new(
        LibraryRepository::open(profile.path().join("library.sqlite"))
            .expect("open temporary library"),
    );
    (profile, repository)
}

fn jpeg_thumbnail() -> Vec<u8> {
    jpeg_thumbnail_with_tint(0x31)
}

fn jpeg_thumbnail_with_tint(tint: u8) -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
        96,
        64,
        Rgba([tint, 0x79, 0xb5, 0xff]),
    ))
    .write_to(&mut encoded, image::ImageFormat::Jpeg)
    .expect("encode card thumbnail fixture");
    encoded.into_inner()
}

fn large_jpeg_thumbnail_with_tint(tint: u8) -> Vec<u8> {
    // Keep this materially larger than the on-disk card proxy.  The mounted
    // A → B → A path below must distinguish reusing an existing 192px proxy
    // from merely passing through a tiny source fixture.
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
        .expect("encode large card thumbnail fixture");
    let bytes = encoded.into_inner();
    assert!(
        bytes.len() > super::card_thumbnail::CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES,
        "the mounted fixture must be larger than a retained card proxy"
    );
    bytes
}

fn card_document(snippet: &str, thumbnail: Option<ResourceId>) -> CanonicalDocument {
    let mut blocks = vec![Block::Paragraph {
        style: BlockStyle::default(),
        inlines: vec![Inline::Text {
            text: snippet.into(),
            marks: Default::default(),
        }],
    }];
    if let Some(resource_id) = thumbnail {
        blocks.push(Block::Image {
            resource_id,
            alt: "卡片封面".into(),
            presentation: Default::default(),
        });
    }
    CanonicalDocument::from_blocks(blocks)
}

fn card_text_selector(part: &str, note_id: &NoteId) -> String {
    format!("library-note-card-{part}-{}", note_id.as_str())
}

fn leak_selector(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn assert_bounds_inside(
    child: gpui::Bounds<gpui::Pixels>,
    row: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    assert!(
        child.left() >= row.left()
            && child.right() <= row.right()
            && child.top() >= row.top()
            && child.bottom() <= row.bottom(),
        "{label} must remain inside its own fixed Cards row: child={child:?}, row={row:?}"
    );
}

fn assert_painted_text_budget(
    title: &super::note_card::PaintedCardText,
    snippet: &super::note_card::PaintedCardText,
    time: &super::note_card::PaintedCardText,
    row: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    // This probe is recorded by the actual production `StyledText` element
    // after GPUI has shaped and painted it. It is intentionally stronger than
    // a wrapper div's debug bounds: removing `line_clamp` keeps the fixed
    // wrapper geometry but produces more shaped visible lines here.
    assert!(
        title.painted_line_count <= 1,
        "{label} title must paint no more than one shaped line: {title:?}"
    );
    assert!(
        snippet.painted_line_count <= 2,
        "{label} snippet must paint no more than two shaped lines: {snippet:?}"
    );
    assert!(
        time.painted_line_count <= 1,
        "{label} footer must paint one shaped line: {time:?}"
    );
    assert!(
        !title.painted_text.is_empty()
            && !snippet.painted_text.is_empty()
            && !time.painted_text.is_empty(),
        "{label} text hierarchy must paint actual visible glyph content: title={title:?}, snippet={snippet:?}, time={time:?}"
    );
    assert!(
        title.line_height == px(20.0)
            && snippet.line_height == px(16.0)
            && time.line_height == px(16.0),
        "{label} must retain the Cards text hierarchy: title={title:?}, snippet={snippet:?}, time={time:?}"
    );
    assert!(
        title.bounds.bottom() <= snippet.bounds.top()
            && snippet.bounds.bottom() <= time.bounds.top(),
        "{label} painted title/snippet/footer must not overlap: title={title:?}, snippet={snippet:?}, time={time:?}"
    );
    assert_bounds_inside(title.bounds, row, &format!("{label} painted title"));
    assert_bounds_inside(snippet.bounds, row, &format!("{label} painted snippet"));
    assert_bounds_inside(time.bounds, row, &format!("{label} painted time"));
}

fn assert_painted_snippet_budget(
    title: &super::note_card::PaintedCardText,
    snippet: &super::note_card::PaintedCardText,
    time: &super::note_card::PaintedCardText,
    row: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    assert!(
        title.painted_line_count <= 1,
        "{label} title must remain one shaped line in the fixed Snippets row: {title:?}"
    );
    assert!(
        snippet.painted_line_count <= 1,
        "{label} preview must remain one shaped line in the fixed Snippets row: {snippet:?}"
    );
    assert_eq!(
        time.painted_line_count, 1,
        "{label} relative date must paint one shaped footer line: {time:?}"
    );
    assert!(
        !title.painted_text.is_empty()
            && !snippet.painted_text.is_empty()
            && !time.painted_text.is_empty(),
        "{label} title, preview, and relative date must all paint glyphs: title={title:?}, snippet={snippet:?}, time={time:?}"
    );
    assert!(
        title.painted_text.ends_with('…') && snippet.painted_text.ends_with('…'),
        "{label} long title and preview must visibly truncate rather than relying on a fixed wrapper crop: title={title:?}, snippet={snippet:?}"
    );
    assert!(
        title.bounds.bottom() <= snippet.bounds.top()
            && snippet.bounds.bottom() <= time.bounds.top(),
        "{label} title, preview, and date must not overlap: title={title:?}, snippet={snippet:?}, time={time:?}"
    );
    assert_bounds_inside(title.bounds, row, &format!("{label} title"));
    assert_bounds_inside(snippet.bounds, row, &format!("{label} preview"));
    assert_bounds_inside(time.bounds, row, &format!("{label} relative date"));
}

#[gpui::test]
async fn mounted_snippets_budget_long_chinese_text_and_date_inside_adjacent_rows_at_release_frame(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: the Release screenshot had fixed 88pt Snippets rows
    // but no date node and no text budget. Removing the production footer or
    // visible text-overflow truncation must make the same production StyledText
    // shape below lose its hierarchy or render the unclipped source text.
    for list_width in [280_u16, 360_u16] {
        let (paint_probe, _paint_scope) = super::note_card::observe_card_text_paints_for_test();
        let (_profile, repository) = repository();
        let leading = repository
            .create_note(CreateNote {
                title: "摘要相邻行一：长中文标题不能挤掉正文或日期".repeat(3),
                notebook_id: None,
                document: card_document(
                    "第一篇摘要在窄列表中必须保留一行预览和相对日期，不能跨越自己的固定行。"
                        .repeat(8)
                        .as_str(),
                    None,
                ),
            })
            .expect("create leading Snippets row");
        let selected = repository
            .create_note(CreateNote {
                title: "验收笔记三·拖拽照片·日期必须在摘要模式的窄列表中可见".repeat(3),
                notebook_id: None,
                document: card_document(
                    "第三篇包含长中文正文预览；摘要模式必须显示足够内容让用户辨认笔记，同时把相对日期留在同一行内。"
                        .repeat(8)
                        .as_str(),
                    None,
                ),
            })
            .expect("create selected Snippets row");
        let trailing = repository
            .create_note(CreateNote {
                title: "摘要相邻行二：下一行不能被前一行的文字遮住".repeat(3),
                notebook_id: None,
                document: card_document(
                    "最后一篇预览也是无硬换行的中文文本，用于证明软换行会被真实布局限制而不会覆盖下一行。"
                        .repeat(8)
                        .as_str(),
                    None,
                ),
            })
            .expect("create trailing Snippets row");
        repository
            .write_library_shell_state(&LibraryShellState {
                sidebar_width: LibraryShellState::DEFAULT_SIDEBAR_WIDTH,
                list_width,
                sidebar_visible: true,
                list_visible: true,
                selected_note_id: Some(selected.id.clone()),
            })
            .expect("persist narrow Snippets list width and selected note");

        let (view, cx) = mount_shell(Arc::clone(&repository), cx);
        cx.simulate_resize(gpui::size(px(1_160.0), px(789.0)));
        cx.update(|window, app| {
            view.update(app, |shell, shell_cx| {
                shell.apply_action(
                    AppAction::SetListViewMode(ListViewMode::Snippets),
                    window,
                    shell_cx,
                );
            });
        });
        for _ in 0..3 {
            redraw(cx);
        }

        let projections = view.read_with(cx, |shell, app| {
            assert_eq!(
                shell.model.read(app).list_view_mode(),
                ListViewMode::Snippets
            );
            shell.model.read(app).projections().to_vec()
        });
        let leading_index = projections
            .iter()
            .position(|projection| projection.id == leading.id)
            .expect("leading projection");
        let trailing_index = projections
            .iter()
            .position(|projection| projection.id == trailing.id)
            .expect("trailing projection");
        let selected_row = cx
            .debug_bounds("library-selected-note-card")
            .expect("selected Snippets row");
        let leading_row = cx
            .debug_bounds(leak_selector(format!("library-note-card-{leading_index}")))
            .expect("leading Snippets row");
        let trailing_row = cx
            .debug_bounds(leak_selector(format!("library-note-card-{trailing_index}")))
            .expect("trailing Snippets row");
        assert_eq!(
            selected_row.size.height,
            px(super::note_card::fixed_height(ListViewMode::Snippets)),
            "Snippets rows must preserve their fixed height at {list_width}px list width"
        );
        assert!(
            leading_row.bottom() <= selected_row.top()
                || selected_row.bottom() <= leading_row.top(),
            "selected Snippets row must not overlap its leading neighbor at {list_width}px"
        );
        assert!(
            trailing_row.bottom() <= selected_row.top()
                || selected_row.bottom() <= trailing_row.top(),
            "selected Snippets row must not overlap its trailing neighbor at {list_width}px"
        );

        for (note, row, label) in [
            (&leading, leading_row, "leading Snippets row"),
            (&selected, selected_row, "selected Snippets row"),
            (&trailing, trailing_row, "trailing Snippets row"),
        ] {
            let probe = paint_probe.borrow();
            let title = probe
                .get(&super::note_card::snippet_text_selector(
                    "title",
                    note.id.as_str(),
                ))
                .unwrap_or_else(|| panic!("{label} title must be shaped by production StyledText"));
            let snippet = probe
                .get(&super::note_card::snippet_text_selector(
                    "snippet",
                    note.id.as_str(),
                ))
                .unwrap_or_else(|| {
                    panic!("{label} preview must be shaped by production StyledText")
                });
            let time = probe
                .get(&super::note_card::snippet_text_selector(
                    "time",
                    note.id.as_str(),
                ))
                .unwrap_or_else(|| panic!("{label} must keep a visible relative-date footer"));
            assert_painted_snippet_budget(title, snippet, time, row, label);
        }
    }
}

#[gpui::test]
async fn mounted_cards_clamp_long_chinese_text_inside_adjacent_rows_at_narrow_widths(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: the Release regression came from an unconstrained
    // text column being vertically centered next to a fixed 76pt thumbnail.
    // Removing the production line clamps, fixed text budget, or title/footer
    // placement must make one of the actual production TextLayout assertions
    // below fail. The snippets deliberately have no hard newlines: GPUI's
    // `line_clamp` bounds soft wraps, which is the behavior users see in a
    // narrow card.
    for list_width in [280_u16, 360_u16] {
        let (paint_probe, _paint_scope) = super::note_card::observe_card_text_paints_for_test();
        let (_profile, repository) = repository();
        let leading = repository
            .create_note(CreateNote {
                title: "相邻卡片一：正文预览不得越过本行".into(),
                notebook_id: None,
                document: card_document(
                    "第一篇摘要在很窄的列表里必须软换行但不能越过自己的两行预算。"
                        .repeat(8)
                        .as_str(),
                    None,
                ),
            })
            .expect("create leading neighboring card");
        let thumbnail_id = repository
            .import_resource(&jpeg_thumbnail(), "窄宽卡片封面.jpeg", "image/jpeg", "jpeg")
            .expect("store real 76pt thumbnail fixture");
        let selected = repository
            .create_note(CreateNote {
                title: "验收笔记三·插图附件·很长的中文标题必须在窄列表中保持一行可读".into(),
                notebook_id: None,
                document: card_document(
                    "第三篇准备插入 JPEG，摘要必须在窄列表中软换行，并且第三行不能压住日期或相邻卡片。"
                        .repeat(7)
                        .as_str(),
                    Some(thumbnail_id),
                ),
            })
            .expect("create selected long Chinese card");
        let trailing = repository
            .create_note(CreateNote {
                title: "相邻卡片二：下一行不能被上方摘要覆盖".into(),
                notebook_id: None,
                document: card_document(
                    "下一篇摘要仍然必须保持在自己的两行预算内，不能覆盖选中卡片或下一行。"
                        .repeat(7)
                        .as_str(),
                    None,
                ),
            })
            .expect("create trailing neighboring card");
        repository
            .write_library_shell_state(&LibraryShellState {
                sidebar_width: LibraryShellState::DEFAULT_SIDEBAR_WIDTH,
                list_width,
                sidebar_visible: true,
                list_visible: true,
                selected_note_id: Some(selected.id.clone()),
            })
            .expect("persist narrow Cards list width and selected note");

        let (view, cx) = mount_shell(Arc::clone(&repository), cx);
        cx.simulate_resize(gpui::size(px(1_180.0), px(700.0)));
        for _ in 0..4 {
            redraw(cx);
        }

        let projections = view.read_with(cx, |shell, app| {
            shell.model.read(app).projections().to_vec()
        });
        let leading_index = projections
            .iter()
            .position(|projection| projection.id == leading.id)
            .expect("leading projection");
        let trailing_index = projections
            .iter()
            .position(|projection| projection.id == trailing.id)
            .expect("trailing projection");
        let selected_row = cx
            .debug_bounds("library-selected-note-card")
            .expect("selected Cards row");
        let leading_row = cx
            .debug_bounds(leak_selector(format!("library-note-card-{leading_index}")))
            .expect("leading neighboring Cards row");
        let trailing_row = cx
            .debug_bounds(leak_selector(format!("library-note-card-{trailing_index}")))
            .expect("trailing neighboring Cards row");
        assert_eq!(
            selected_row.size.height,
            px(super::note_card::fixed_height(ListViewMode::Cards)),
            "Cards rows must keep their virtual-list fixed height at {list_width}px width"
        );
        assert!(
            leading_row.bottom() <= selected_row.top()
                || selected_row.bottom() <= leading_row.top(),
            "the selected row must not overlap its leading neighbor at {list_width}px"
        );
        assert!(
            trailing_row.bottom() <= selected_row.top()
                || selected_row.bottom() <= trailing_row.top(),
            "the selected row must not overlap its trailing neighbor at {list_width}px"
        );

        for (note, row, card_label) in [
            (&leading, leading_row, "leading text-first card"),
            (&selected, selected_row, "selected thumbnail card"),
            (&trailing, trailing_row, "trailing text-first card"),
        ] {
            let probe = paint_probe.borrow();
            let title = probe
                .get(&card_text_selector("title", &note.id))
                .unwrap_or_else(|| {
                    panic!("{card_label} title must be shaped by the production text element")
                });
            let snippet = probe
                .get(&card_text_selector("snippet", &note.id))
                .unwrap_or_else(|| {
                    panic!("{card_label} snippet must be shaped by the production text element")
                });
            let time = probe
                .get(&card_text_selector("time", &note.id))
                .unwrap_or_else(|| {
                    panic!("{card_label} time must be shaped by the production text element")
                });
            assert_painted_text_budget(title, snippet, time, row, card_label);
        }
        let thumbnail = cx
            .debug_bounds("library-note-card-thumbnail")
            .expect("selected Cards image uses the real thumbnail slot");
        assert_eq!(thumbnail.size.width, px(76.0));
        assert_eq!(thumbnail.size.height, px(76.0));
        assert_bounds_inside(thumbnail, selected_row, "76pt thumbnail");
    }
}

#[gpui::test]
async fn mounted_cards_materialize_only_the_uniform_list_visible_thumbnail_set(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: replacing the range-derived desired set with every
    // projection would leave this test with 192 retained/queued resources.
    // The actual list must keep the data path projection-only and defer all
    // descriptor verification to the small current Card range.
    let (_profile, repository) = repository();
    const CARD_COUNT: usize = 192;
    for index in 0..CARD_COUNT {
        let tint = u8::try_from(index).expect("fixture tint fits in u8");
        let filename = format!("card-{index}.jpeg");
        let resource_id = repository
            .import_resource(
                &jpeg_thumbnail_with_tint(tint),
                &filename,
                "image/jpeg",
                "jpeg",
            )
            .expect("store distinct card thumbnail");
        repository
            .create_note(CreateNote {
                title: format!("缩略图卡片 {index:02}"),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Image {
                    resource_id,
                    alt: "卡片缩略图".into(),
                    presentation: Default::default(),
                }]),
            })
            .expect("create covered note");
    }

    let body_loads = repository.observe_note_loads();
    let blob_reads = repository.observe_resource_reads();
    let model = cx.new(|_| AppModel::open(Arc::clone(&repository)).expect("open model"));
    let thumbnail_gate = Arc::new(Mutex::new(None));
    let gate_slot = Arc::clone(&thumbnail_gate);
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut shell = LibraryShell::new(model, None, window, cx);
        *gate_slot.lock().expect("thumbnail gate slot poisoned") =
            Some(shell.stall_next_card_thumbnail_materialization_for_test());
        shell
    });
    let _release = thumbnail_gate
        .lock()
        .expect("thumbnail gate slot poisoned")
        .take()
        .expect("the mounted shell must own the first thumbnail worker gate");
    // The retained worker is genuinely scheduled from the production render
    // path but cannot complete into an all-card cascade before this bounded
    // residency assertion has inspected the shell.
    redraw(cx);

    let (range, desired, queued, ready) = view.read_with(cx, |shell, _| {
        (
            shell
                .rendered_note_range
                .clone()
                .expect("uniform list should report its rendered range"),
            shell.card_thumbnails.desired_count_for_test(),
            shell.card_thumbnails.queued_count_for_test(),
            shell.card_thumbnails.ready_count_for_test(),
        )
    });
    assert!(
        range.end < CARD_COUNT,
        "a {CARD_COUNT}-card list must retain a bounded viewport range, got {range:?}"
    );
    assert!(
        desired == range.len() && desired < CARD_COUNT,
        "card thumbnail work escaped the mounted range: desired={desired}, range={range:?}"
    );
    assert!(
        queued < CARD_COUNT && queued <= desired,
        "the staged card queue must be rebuilt from the final viewport, not accumulate a full-list backlog: queued={queued}, desired={desired}"
    );
    assert!(
        ready <= desired,
        "only requested Cards sources may be adopted into the retained thumbnail cache"
    );
    assert_eq!(
        body_loads.try_recv(),
        Err(TryRecvError::Empty),
        "a Cards viewport must not hydrate note bodies"
    );
    assert_eq!(
        blob_reads.try_recv(),
        Err(TryRecvError::Empty),
        "a Cards viewport must not use the Vec-based resource read API"
    );
}

#[gpui::test]
async fn mounted_cards_scrolled_tail_does_not_schedule_the_measurement_probe_thumbnail(
    cx: &mut TestAppContext,
) {
    // GPUI measures item zero twice before it asks uniform_list for the real
    // viewport. This must be a production scroll-to-tail check, not merely a
    // collector unit test: retaining the measurement probe would repeatedly
    // fsync card zero while a person is reading the tail of a large library.
    const CARD_COUNT: usize = 96;
    let (_profile, repository) = repository();
    for index in 0..CARD_COUNT {
        let filename = format!("tail-card-{index}.jpeg");
        let resource_id = repository
            .import_resource(
                &jpeg_thumbnail_with_tint(u8::try_from(index).expect("fixture tint")),
                &filename,
                "image/jpeg",
                "jpeg",
            )
            .expect("store tail card thumbnail");
        repository
            .create_note(CreateNote {
                title: format!("尾部缩略图卡片 {index:02}"),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Image {
                    resource_id,
                    alt: "卡片缩略图".into(),
                    presentation: Default::default(),
                }]),
            })
            .expect("create tail card note");
    }
    let opened = AppModel::open(Arc::clone(&repository)).expect("inspect card projections");
    let first = opened.projections().first().expect("first card").clone();
    let tail_index = opened.projections().len() - 1;
    let tail = opened.projections().last().expect("tail card").clone();
    repository
        .write_library_shell_state(&app_lite_core::LibraryShellState {
            selected_note_id: Some(tail.id.clone()),
            ..app_lite_core::LibraryShellState::default()
        })
        .expect("persist restored tail selection");

    let model = cx.new(|_| AppModel::open(Arc::clone(&repository)).expect("open model"));
    let thumbnail_gate = Arc::new(Mutex::new(None));
    let gate_slot = Arc::clone(&thumbnail_gate);
    let (view, cx) = cx.add_window_view(move |window, cx| {
        let mut shell = LibraryShell::new(model, None, window, cx);
        *gate_slot.lock().expect("thumbnail gate slot poisoned") =
            Some(shell.stall_next_card_thumbnail_materialization_for_test());
        shell
    });
    let _release = thumbnail_gate
        .lock()
        .expect("thumbnail gate slot poisoned")
        .take()
        .expect("the mounted shell must own the first thumbnail worker gate");
    redraw(cx);

    view.read_with(cx, |shell, _| {
        assert!(
            shell
                .rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&tail_index)),
            "the restored tail selection must drive the actual card viewport"
        );
        assert!(
            shell.card_thumbnails.desired_contains_for_test(
                tail.selected_thumbnail_id
                    .as_ref()
                    .expect("tail thumbnail id"),
            ),
            "the final tail viewport must schedule its own thumbnail"
        );
        assert!(
            !shell.card_thumbnails.desired_contains_for_test(
                first
                    .selected_thumbnail_id
                    .as_ref()
                    .expect("first thumbnail id"),
            ),
            "uniform_list measurement item zero must not survive into tail residency"
        );
    });
}

#[gpui::test]
async fn mounted_cards_uniform_list_reuses_bounded_a_proxy_after_viewport_b_round_trip(
    cx: &mut TestAppContext,
) {
    // This is deliberately a real LibraryShell/UniformList round trip rather
    // than a CardThumbnailManager unit path.  The list renderer is the layer
    // that formerly treated GPUI's measurement probe as a viewport change and
    // re-opened/re-copied A after a person read B and returned.
    let (_profile, repository) = repository();
    let bytes_a = large_jpeg_thumbnail_with_tint(0x21);
    let thumbnail_a = repository
        .import_resource(&bytes_a, "round-trip-a.jpeg", "image/jpeg", "jpeg")
        .expect("store large A thumbnail");
    let note_a = repository
        .create_note(CreateNote {
            title: "滚动往返 A".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_a.clone(),
                alt: "A 封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create A card");

    // Keep A and B separated by more than one Cards viewport, while a
    // text-only selected anchor prevents the editor's independent lazy image
    // hydration from contributing descriptor opens to this list assertion.
    for index in 0..96 {
        repository
            .create_note(CreateNote {
                title: format!("滚动间隔文字卡片 {index:03}"),
                notebook_id: None,
                document: CanonicalDocument::default(),
            })
            .expect("create text-only interval card");
    }
    let bytes_b = large_jpeg_thumbnail_with_tint(0x73);
    let thumbnail_b = repository
        .import_resource(&bytes_b, "round-trip-b.jpeg", "image/jpeg", "jpeg")
        .expect("store large B thumbnail");
    let note_b = repository
        .create_note(CreateNote {
            title: "滚动往返 B".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_b.clone(),
                alt: "B 封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create B card");
    let anchor = repository
        .create_note(CreateNote {
            title: "列表滚动锚点（无图）".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create text-only selected anchor");

    let inspection = AppModel::open(Arc::clone(&repository)).expect("inspect card projections");
    let a_index = inspection
        .projections()
        .iter()
        .position(|projection| projection.id == note_a.id)
        .expect("A projection");
    let b_index = inspection
        .projections()
        .iter()
        .position(|projection| projection.id == note_b.id)
        .expect("B projection");
    let (first_image_index, last_image_index) = if a_index < b_index {
        (a_index, b_index)
    } else {
        (b_index, a_index)
    };
    let midpoint = first_image_index + (last_image_index - first_image_index) / 2;
    let text_only_index = ((first_image_index + 1)..last_image_index)
        .filter(|index| {
            inspection
                .projections()
                .get(*index)
                .is_some_and(|projection| projection.selected_thumbnail_id.is_none())
        })
        .min_by_key(|index| index.abs_diff(midpoint))
        .expect("the A/B fixture must include a real text-only viewport between the images");
    assert!(
        a_index.abs_diff(b_index) > 32,
        "the fixture must require a real viewport transition"
    );
    repository
        .write_library_shell_state(&app_lite_core::LibraryShellState {
            selected_note_id: Some(anchor.id),
            ..app_lite_core::LibraryShellState::default()
        })
        .expect("persist text-only anchor selection");

    let verified_opens = repository.observe_verified_resource_opens();
    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    redraw(cx);

    let scroll_to = |index, cx: &mut VisualTestContext| {
        view.update(cx, |shell, _| {
            shell
                .note_list_scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Center);
        });
        for _ in 0..6 {
            redraw(cx);
        }
    };

    scroll_to(a_index, cx);
    let a_source = view.read_with(cx, |shell, _| {
        assert!(
            shell
                .rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&a_index)),
            "A must be reached through the production uniform-list viewport"
        );
        shell
            .card_thumbnails
            .source_for(Some(&thumbnail_a))
            .expect("A must materialize a card proxy")
    });
    assert!(
        std::fs::metadata(&a_source)
            .expect("A proxy metadata")
            .len()
            <= u64::try_from(super::card_thumbnail::CARD_THUMBNAIL_PROXY_MAX_ENCODED_BYTES)
                .expect("proxy bound fits u64"),
        "the mounted Cards source must be the bounded proxy, never the original JPEG"
    );
    view.read_with(cx, |shell, app| {
        let cache = shell.card_thumbnail_cache.read(app);
        assert!(cache.is_settled(), "A card proxy decode must settle");
        assert!(
            cache.loaded_success_for_test(&Resource::from(a_source.clone())),
            "A must be a decoded card bitmap, not only a staged manager lease"
        );
        assert_eq!(cache.len(), 1, "only the actual A viewport may be decoded");
    });

    scroll_to(text_only_index, cx);
    view.read_with(cx, |shell, app| {
        let rendered_range = shell
            .rendered_note_range
            .clone()
            .expect("the text-only viewport must render a bounded range");
        assert!(
            rendered_range.contains(&text_only_index),
            "the real UniformList must enter the text-only middle viewport"
        );
        assert!(
            !rendered_range.contains(&a_index) && !rendered_range.contains(&b_index),
            "the middle viewport must exclude both image cards, got {rendered_range:?}"
        );
        assert!(
            rendered_range.clone().all(|index| {
                inspection
                    .projections()
                    .get(index)
                    .is_some_and(|projection| projection.selected_thumbnail_id.is_none())
            }),
            "the midpoint fixture must be a genuinely text-only Cards range, got {rendered_range:?}"
        );
        assert_eq!(
            shell.card_thumbnails.desired_count_for_test(),
            0,
            "a Cards viewport containing only text projections has no thumbnail residency target; range={rendered_range:?}, contains_a={}, contains_b={}",
            rendered_range.contains(&a_index),
            rendered_range.contains(&b_index),
        );
        assert_eq!(
            shell.card_thumbnails.source_for(Some(&thumbnail_a)),
            Some(a_source.clone()),
            "a text-only Cards viewport must retain the bounded A proxy for a nearby return"
        );
        assert_eq!(
            shell.card_thumbnail_cache.read(app).len(),
            0,
            "the text-only viewport must still release the decoded card texture"
        );
    });

    scroll_to(b_index, cx);
    let b_source = view.read_with(cx, |shell, _| {
        assert!(
            shell
                .rendered_note_range
                .as_ref()
                .is_some_and(|range| range.contains(&b_index)),
            "B must be reached through the production uniform-list viewport"
        );
        shell
            .card_thumbnails
            .source_for(Some(&thumbnail_b))
            .expect("B must materialize a card proxy")
    });
    assert_ne!(a_source, b_source, "A and B must retain separate leases");
    view.read_with(cx, |shell, app| {
        let cache = shell.card_thumbnail_cache.read(app);
        assert!(cache.is_settled(), "B card proxy decode must settle");
        assert!(
            cache.loaded_success_for_test(&Resource::from(b_source.clone())),
            "B must be a decoded card bitmap, not only a staged manager lease"
        );
        assert_eq!(
            cache.len(),
            1,
            "the B viewport must evict A's decoded texture"
        );
    });

    scroll_to(text_only_index, cx);
    view.read_with(cx, |shell, _| {
        assert_eq!(
            shell.card_thumbnails.source_for(Some(&thumbnail_a)),
            Some(a_source.clone()),
            "the intermediate text viewport must not discard A after B loads"
        );
        assert_eq!(
            shell.card_thumbnails.source_for(Some(&thumbnail_b)),
            Some(b_source.clone()),
            "the intermediate text viewport must not discard B either"
        );
    });

    scroll_to(a_index, cx);
    view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.card_thumbnails.source_for(Some(&thumbnail_a)),
            Some(a_source.clone()),
            "returning to A must reuse its retained proxy rather than rematerialize it"
        );
        assert!(
            shell.card_thumbnails.source_bytes_for_test()
                <= super::card_thumbnail::CARD_THUMBNAIL_SOURCE_CACHE_BUDGET,
            "the actual LibraryShell viewport cache must remain within its strict source budget"
        );
        assert!(
            shell.card_thumbnails.ready_count_for_test() <= 2,
            "only A and B may remain after the round trip, not a scrolling history"
        );
        let cache = shell.card_thumbnail_cache.read(app);
        assert!(cache.is_settled(), "returning A decode must settle");
        assert!(
            cache.loaded_success_for_test(&Resource::from(a_source.clone())),
            "returning A must decode its retained proxy rather than a failed/placeholder result"
        );
        assert_eq!(
            cache.len(),
            1,
            "only the final A texture may remain decoded"
        );
    });
    assert_eq!(
        verified_opens.try_iter().count(),
        2,
        "the real A → B → A renderer must hash/open each large resource once, not reopen A after viewport return"
    );
}

#[gpui::test]
async fn mounted_cards_stable_viewport_reaches_idle_without_reinstalling_thumbnail_cache(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: UniformList measures then paints the same range.
    // Reinstalling an unchanged cache source set from every deferred range
    // marks the cache entity dirty, so `run_until_parked` cannot settle and
    // repeatedly decodes the same private 192px proxy.
    let (_profile, repository) = repository();
    let thumbnail_id = repository
        .import_resource(&jpeg_thumbnail(), "稳定视口.jpeg", "image/jpeg", "jpeg")
        .expect("store thumbnail fixture");
    repository
        .create_note(CreateNote {
            title: "稳定卡片视口".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_id,
                alt: "稳定卡片封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create covered note");
    let verified_opens = repository.observe_verified_resource_opens();
    let (view, cx) = mount_shell(repository, cx);

    for _ in 0..4 {
        redraw(cx);
    }
    let (initial_cache_reconciliations, initial_viewport_reconciliations) =
        view.read_with(cx, |shell, _| {
            assert_eq!(
                shell.card_thumbnails.ready_count_for_test(),
                1,
                "the stable visible card must adopt one bounded source"
            );
            (
                shell.card_thumbnail_cache_reconciliations_for_test(),
                shell.card_thumbnail_viewport_reconciliations_for_test(),
            )
        });
    assert!(
        initial_cache_reconciliations > 0,
        "the completed source must be installed into the dedicated card cache once"
    );

    for _ in 0..3 {
        redraw(cx);
    }
    let (stable_cache_reconciliations, stable_viewport_reconciliations) =
        view.read_with(cx, |shell, _| {
            (
                shell.card_thumbnail_cache_reconciliations_for_test(),
                shell.card_thumbnail_viewport_reconciliations_for_test(),
            )
        });
    assert_eq!(
        stable_cache_reconciliations, initial_cache_reconciliations,
        "a fixed viewport must park without re-installing unchanged cache residency"
    );
    assert_eq!(
        stable_viewport_reconciliations, initial_viewport_reconciliations,
        "a fixed viewport must not re-enter thumbnail residency reconciliation from every prepaint"
    );
    assert_eq!(
        verified_opens.try_iter().count(),
        1,
        "stable redraws must not restart descriptor-safe card materialization"
    );
}

fn mount_shell<'a>(
    repository: Arc<LibraryRepository>,
    cx: &'a mut TestAppContext,
) -> (Entity<LibraryShell>, &'a mut VisualTestContext) {
    let model = cx.new(|_| AppModel::open(repository).expect("open model"));
    cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx))
}

#[gpui::test]
async fn mounted_text_only_cards_do_not_render_an_empty_thumbnail_tile(cx: &mut TestAppContext) {
    // Mutation-sensitive: the placeholder belongs to an unresolved *actual*
    // thumbnail key. Giving every text card a gray 76pt square makes the
    // title-first card mode visually misleading and this selector fails.
    let (_profile, repository) = repository();
    repository
        .create_note(CreateNote {
            title: "只有文字的卡片".into(),
            notebook_id: None,
            document: CanonicalDocument::default(),
        })
        .expect("create text-only note");
    let (_view, cx) = mount_shell(repository, cx);
    redraw(cx);

    assert!(cx.debug_bounds("library-note-card-cards").is_some());
    assert!(
        cx.debug_bounds("library-note-card-thumbnail").is_none()
            && cx
                .debug_bounds("library-note-card-thumbnail-placeholder")
                .is_none(),
        "a note without selected_thumbnail_id must remain a text-first card"
    );
}

#[gpui::test]
async fn mounted_cards_render_selected_thumbnail_without_projection_body_or_blob_reads(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: the SQL projection already carries the selected
    // image ResourceId. Rendering that key must produce a real thumbnail
    // element, while the initial route/query stage remains body/blob-free.
    // Reverting card rendering to its former "有缩略图" label makes the
    // selector assertion below fail.
    let (_profile, repository) = repository();
    let thumbnail_id = repository
        .import_resource(&jpeg_thumbnail(), "卡片封面.jpeg", "image/jpeg", "jpeg")
        .expect("store JPEG thumbnail fixture");
    repository
        .create_note(CreateNote {
            title: "带真实封面的卡片".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_id.clone(),
                alt: "卡片封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create covered note");

    let verified_opens = repository.observe_verified_resource_opens();
    let body_loads = repository.observe_note_loads();
    let blob_reads = repository.observe_resource_reads();
    let model_state = AppModel::open(Arc::clone(&repository)).expect("open model");

    assert_eq!(
        verified_opens.try_recv(),
        Err(TryRecvError::Empty),
        "opening the model/query projection must not eagerly hydrate a card resource"
    );
    assert_eq!(
        body_loads.try_recv(),
        Err(TryRecvError::Empty),
        "a list projection must not load the note body"
    );
    assert_eq!(
        blob_reads.try_recv(),
        Err(TryRecvError::Empty),
        "a list projection must not call the Vec-based blob read API"
    );

    let model = cx.new(|_| model_state);
    let (view, cx) =
        cx.add_window_view(move |window, cx| LibraryShell::new(model, None, window, cx));
    // The mounted visible Cards range is now allowed to schedule its retained
    // worker. A few deterministic frame turns cover verification, source
    // adoption, and the cache's background proxy decode without sleeps.
    for _ in 0..4 {
        redraw(cx);
    }

    assert!(
        cx.debug_bounds("library-note-card-cards").is_some(),
        "the real default list route must be Cards for this presentation check"
    );
    assert!(
        cx.debug_bounds("library-note-card-thumbnail").is_some(),
        "a selected_thumbnail_id must render a thumbnail, not merely a status label"
    );
    let source = view.read_with(cx, |shell, app| {
        assert_eq!(
            shell.card_thumbnails.ready_count_for_test(),
            1,
            "only the selected card thumbnail source should be adopted"
        );
        assert!(
            shell.card_thumbnail_cache.read(app).is_settled(),
            "the thumbnail proxy must decode through the bounded background cache"
        );
        assert_eq!(
            shell.card_thumbnail_cache.read(app).len(),
            1,
            "the real JPEG card must enter exactly one dedicated thumbnail cache entry"
        );
        shell
            .card_thumbnails
            .source_for(Some(&thumbnail_id))
            .expect("visible card must retain its adopted private source")
    });
    assert!(
        source.is_file(),
        "the active card lease must own its source"
    );
    let decoded_ok = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            let resource = Resource::from(source.clone());
            shell
                .card_thumbnail_cache
                .update(shell_cx, |cache, cache_cx| {
                    matches!(cache.load(&resource, window, cache_cx), Some(Ok(_)))
                })
        })
    });
    assert!(
        decoded_ok,
        "the visible card selector must correspond to a decoded RenderImage, not Loaded(Err)"
    );
    assert!(
        verified_opens.try_iter().count() == 1,
        "a real card thumbnail must reuse one verified descriptor for inspection and bounded proxy staging"
    );
    assert_eq!(
        body_loads.try_recv(),
        Err(TryRecvError::Empty),
        "card rendering must remain projection-only and never hydrate canonical bodies"
    );
    assert_eq!(
        blob_reads.try_recv(),
        Err(TryRecvError::Empty),
        "card rendering must never call the Vec-based blob read API"
    );

    // Switching presentation modes is also a residency transition. It must
    // release the list-only raw source/cache without touching the editor's
    // independent image store.
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SetListViewMode(ListViewMode::Snippets),
                window,
                shell_cx,
            );
        });
    });
    for _ in 0..2 {
        redraw(cx);
    }
    view.read_with(cx, |shell, app| {
        assert_eq!(shell.card_thumbnails.desired_count_for_test(), 0);
        assert_eq!(shell.card_thumbnails.ready_count_for_test(), 0);
        assert_eq!(
            shell.card_thumbnail_cache.read(app).len(),
            0,
            "leaving Cards must evict its independent decoded proxy"
        );
    });
    assert!(
        !source.exists(),
        "leaving Cards must drop the task-owned thumbnail source lease"
    );
}

#[gpui::test]
async fn mounted_card_thumbnail_failure_is_visible_and_recovers_after_viewport_reentry(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: a failed descriptor materialization must not remain
    // indistinguishable from a normal loading tile, nor retry from every
    // repaint. Restoring the durable bytes only earns a new attempt after the
    // visible card leaves and re-enters the Cards working set.
    let (profile, repository) = repository();
    let bytes = jpeg_thumbnail();
    let thumbnail_id = repository
        .import_resource(&bytes, "损坏封面.jpeg", "image/jpeg", "jpeg")
        .expect("store thumbnail fixture");
    repository
        .create_note(CreateNote {
            title: "可恢复缩略图失败".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_id.clone(),
                alt: "损坏封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create covered note");
    let metadata = repository
        .resource_metadata(&thumbnail_id)
        .expect("load resource metadata")
        .expect("stored thumbnail metadata");
    let blob_path = profile
        .path()
        .join("resources")
        .join("blobs")
        .join(metadata.sha256.as_str());
    std::fs::remove_file(&blob_path).expect("remove fixture blob after its metadata is committed");

    let (view, cx) = mount_shell(Arc::clone(&repository), cx);
    for _ in 0..3 {
        redraw(cx);
    }
    assert!(
        cx.debug_bounds("library-note-card-thumbnail-error")
            .is_some(),
        "a failed selected-thumbnail materialization must give Cards a visible, specific error state"
    );

    // Restoring bytes alone must not turn every later frame into a retry loop.
    std::fs::write(&blob_path, &bytes).expect("restore validated fixture blob");
    for _ in 0..2 {
        redraw(cx);
    }
    assert!(
        cx.debug_bounds("library-note-card-thumbnail-error")
            .is_some(),
        "the failed card stays visible until a real viewport transition requests one retry"
    );

    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SetListViewMode(ListViewMode::Snippets),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SetListViewMode(ListViewMode::Cards),
                window,
                shell_cx,
            );
        });
    });
    for _ in 0..4 {
        redraw(cx);
    }
    assert!(
        cx.debug_bounds("library-note-card-thumbnail").is_some(),
        "leaving and re-entering Cards must retry the repaired resource once"
    );
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.card_thumbnails.failed(Some(&thumbnail_id)),
            "a successful retry must clear the manager's completed failure state"
        );
        assert!(
            shell
                .card_thumbnails
                .source_for(Some(&thumbnail_id))
                .is_some(),
            "the repaired resource must replace the failure with a retained proxy"
        );
    });
}

#[gpui::test]
async fn mounted_card_proxy_decode_failure_is_visible_instead_of_a_neutral_tile(
    cx: &mut TestAppContext,
) {
    // Mutation-sensitive: if a retained card proxy becomes unreadable after
    // materialization, `BudgetedImageCache` reaches Loaded(Err). Cards must
    // surface that completed failure rather than continue to paint the normal
    // thumbnail frame behind a neutral background.
    let (_profile, repository) = repository();
    let thumbnail_id = repository
        .import_resource(&jpeg_thumbnail(), "缓存损坏封面.jpeg", "image/jpeg", "jpeg")
        .expect("store thumbnail fixture");
    repository
        .create_note(CreateNote {
            title: "代理缓存失败可见".into(),
            notebook_id: None,
            document: CanonicalDocument::from_blocks(vec![Block::Image {
                resource_id: thumbnail_id.clone(),
                alt: "代理缓存封面".into(),
                presentation: Default::default(),
            }]),
        })
        .expect("create covered note");
    let (view, cx) = mount_shell(repository, cx);
    for _ in 0..4 {
        redraw(cx);
    }
    let source = view.read_with(cx, |shell, _| {
        shell
            .card_thumbnails
            .source_for(Some(&thumbnail_id))
            .expect("visible card proxy")
    });
    let original_proxy = std::fs::read(&source).expect("read valid private proxy fixture");
    std::fs::write(&source, b"not a card image").expect("corrupt private proxy fixture");
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell
                .card_thumbnail_cache
                .update(shell_cx, |cache, cache_cx| {
                    cache.invalidate(&Resource::from(source.clone()), window, cache_cx);
                });
        });
    });
    for _ in 0..3 {
        redraw(cx);
    }

    assert!(
        cx.debug_bounds("library-note-card-thumbnail-error")
            .is_some(),
        "a Loaded(Err) card cache entry must render the explicit failure tile"
    );

    std::fs::write(&source, original_proxy).expect("restore private proxy fixture");
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SetListViewMode(ListViewMode::Snippets),
                window,
                shell_cx,
            );
        });
    });
    redraw(cx);
    cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            shell.apply_action(
                AppAction::SetListViewMode(ListViewMode::Cards),
                window,
                shell_cx,
            );
        });
    });
    for _ in 0..3 {
        redraw(cx);
    }
    let recovered_source = view.read_with(cx, |shell, _| {
        shell
            .card_thumbnails
            .source_for(Some(&thumbnail_id))
            .expect("Cards re-entry should materialize a fresh private proxy")
    });
    let decode_recovered = cx.update(|window, app| {
        view.update(app, |shell, shell_cx| {
            let resource = Resource::from(recovered_source.clone());
            shell
                .card_thumbnail_cache
                .update(shell_cx, |cache, cache_cx| {
                    matches!(cache.load(&resource, window, cache_cx), Some(Ok(_)))
                })
        })
    });
    assert!(
        decode_recovered,
        "a Cards viewport re-entry must retry a repaired proxy instead of retaining Loaded(Err)"
    );
}
