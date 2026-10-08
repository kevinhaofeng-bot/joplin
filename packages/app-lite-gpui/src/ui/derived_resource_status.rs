//! Current-note extraction diagnostics. Metadata only, with its own redraw
//! boundary: a background OCR backlog must not redraw the whole library.
use app_lite_core::{DerivedTextFailure, DerivedTextJob, DerivedTextStatus, LibraryRepository, Note, NoteId, ResourceId};
use gpui::{Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, px, rgba};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Clone, PartialEq, Eq)]
struct Target {
    note_id: NoteId,
    resources: Vec<ResourceId>,
    deleted: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Row {
    job: DerivedTextJob,
    filename: String,
    status: DerivedTextStatus,
}

pub(super) struct DerivedResourceStatus {
    repository: Arc<LibraryRepository>,
    wake_worker: Arc<AtomicBool>,
    target: Option<Target>,
    generation: u64,
    rows: Vec<Row>,
    overflow: usize,
    expanded: bool,
    retrying: Option<ResourceId>,
    notice: Option<String>,
    _refresh: Option<Task<()>>,
}

impl DerivedResourceStatus {
    pub fn new(repository: Arc<LibraryRepository>, wake_worker: Arc<AtomicBool>) -> Self {
        Self { repository, wake_worker, target: None, generation: 0, rows: Vec::new(), overflow: 0, expanded: false,
            retrying: None, notice: None, _refresh: None }
    }

    pub fn configure(&mut self, note: Option<&Note>, cx: &mut Context<Self>) {
        let same = match (&self.target, note) {
            (None, None) => true,
            (Some(target), Some(note)) => target.note_id == note.id
                && target.resources == note.resource_ids && target.deleted == note.deleted_time.is_some(),
            _ => false,
        };
        if same { return; }
        self.generation = self.generation.wrapping_add(1);
        self.target = note.map(|note| Target { note_id: note.id.clone(), resources: note.resource_ids.clone(), deleted: note.deleted_time.is_some() });
        self.rows.clear();
        self.overflow = 0;
        self.expanded = false;
        self.retrying = None;
        self.notice = None;
        self._refresh = None;
        cx.notify();
        self.refresh(cx);
    }

    pub fn completed(&mut self, resource: &ResourceId, cx: &mut Context<Self>) {
        if self.target.as_ref().is_some_and(|target| target.resources.contains(resource)) {
            self.refresh(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return; };
        if target.resources.is_empty() { return; }
        let repository = Arc::clone(&self.repository);
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let ids = target.resources.clone();
            let result = cx.background_executor().spawn(async move {
                let mut rows = Vec::new();
                let mut overflow = 0;
                for id in ids {
                    let Some(status) = repository.derived_text_status(&id)? else { continue; };
                    if matches!(status, DerivedTextStatus::Indexed { .. }) { continue; }
                    let Some(metadata) = repository.resource_metadata(&id)? else { continue; };
                    if rows.len() == 20 { overflow += 1; continue; }
                    rows.push(Row {
                        job: DerivedTextJob { resource_id: id, sha256: metadata.sha256,
                            extractor_version: app_lite_core::schema::DERIVED_TEXT_EXTRACTOR_VERSION.into() },
                        filename: metadata.title.chars().take(80).collect(), status,
                    });
                }
                Ok::<_, app_lite_core::LibraryError>((rows, overflow))
            }).await;
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation || view.target.as_ref() != Some(&target) { return; }
                match result {
                    Ok((rows, overflow)) => {
                        if view.rows != rows || view.overflow != overflow {
                            view.rows = rows;
                            view.overflow = overflow;
                            cx.notify();
                        }
                    }
                    Err(_) => {
                        view.notice = Some("无法读取附件检索状态；正文和原附件仍然保留。".into());
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn retry(&mut self, job: DerivedTextJob, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return; };
        if target.deleted || self.retrying.is_some() || !self.rows.iter().any(|row|
            row.job == job && matches!(row.status, DerivedTextStatus::Failed { .. })) { return; }
        self.retrying = Some(job.resource_id.clone());
        self.notice = None;
        cx.notify();
        let repository = Arc::clone(&self.repository);
        let wake_worker = Arc::clone(&self.wake_worker);
        cx.spawn(async move |this, cx| {
            let worker_job = job.clone();
            let result = cx.background_executor().spawn(async move {
                let result = repository.retry_derived_text(&worker_job);
                if matches!(result, Ok(true)) { wake_worker.store(true, Ordering::Release); }
                result
            }).await;
            let _ = this.update(cx, |view, cx| {
                if view.target.as_ref() != Some(&target) { return; }
                view.retrying = None;
                match result {
                    Ok(true) => {},
                    Ok(false) => view.notice = Some("附件状态已变化，未重复提交；正在刷新检索状态。".into()),
                    Err(_) => view.notice = Some("重试未提交，正文和原附件仍然保留；可再次重试。".into()),
                }
                view.refresh(cx);
                cx.notify();
            });
        }).detach();
    }
}

fn reason(failure: &DerivedTextFailure) -> &'static str {
    match failure {
        DerivedTextFailure::Unsupported => "此格式暂不支持文字提取",
        DerivedTextFailure::Unavailable => "文字提取服务暂不可用",
        DerivedTextFailure::Parse => "文件无法解析",
        DerivedTextFailure::Locked => "文件已加密或锁定",
        DerivedTextFailure::NoSelectableText => "没有可提取的文字（不代表附件没有内容）",
        DerivedTextFailure::TooLarge => "超过单次提取大小限制",
        DerivedTextFailure::Timeout => "文字提取超时",
        DerivedTextFailure::Failed => "文字提取失败",
    }
}

impl Render for DerivedResourceStatus {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let deleted = self.target.as_ref().is_some_and(|target| target.deleted);
        let multiple = self.rows.len() + self.overflow > 1;
        let mut content = div().w_full().min_w(px(0.0)).flex_none().max_w(px(560.0))
            .flex().flex_col().gap(px(4.0))
            .text_size(px(11.0)).line_height(px(16.0)).text_color(rgba(0x8d6a27ff));
        if multiple {
            content = content.child(
                div().w_full().min_w(px(0.0)).flex().items_center().gap(px(8.0))
                    .child(div().flex_1().min_w(px(0.0)).child(format!(
                        "{} 个附件文字尚未纳入搜索；正文和原附件已保留。",
                        self.rows.len() + self.overflow,
                    )))
                    .child(div().id("library-derived-resource-details-toggle")
                        .debug_selector(|| "library-derived-resource-details-toggle".to_owned())
                        .flex_none().px(px(8.0)).py(px(4.0)).cursor_pointer()
                        .text_color(rgba(0x248a3dff))
                        .on_mouse_down(MouseButton::Left, cx.listener(|view, _, _, cx| {
                            view.expanded = !view.expanded;
                            cx.notify();
                        }))
                        .child(if self.expanded { "收起详情" } else { "查看详情" })),
            );
        }
        // Keep one failure's existing immediate explanation/retry unchanged.
        // Many failures use a bounded, independent scroll owner: the original
        // rows and retry handlers stay real, without filling the writing pane.
        let details = div().id("library-derived-resource-details")
            .debug_selector(|| "library-derived-resource-details".to_owned())
            .w_full().min_w(px(0.0)).flex_none().flex().flex_col().gap(px(8.0))
            .children(self.rows.iter().map(|row| {
                let resource = row.job.resource_id.as_str();
                let failure = matches!(row.status, DerivedTextStatus::Failed { .. });
                let selector = format!("library-derived-resource-{}-{resource}", if failure { "failure" } else { "pending" });
                let retry_selector = format!("library-derived-resource-retry-{resource}");
                let message = match &row.status {
                    DerivedTextStatus::Failed { failure, .. } => format!("{}：{}。正文和原附件已保留；附件文字暂未纳入搜索。", row.filename, reason(failure)),
                    _ => format!("{}：正在后台提取文字；正文和原附件不受影响。", row.filename),
                };
                let job = row.job.clone();
                let busy = self.retrying.is_some();
                div().id(SharedString::from(selector.clone())).debug_selector(move || selector.clone())
                    .w_full().flex_none().flex().items_start().gap(px(8.0))
                    .child(div().flex_1().min_w(px(0.0)).child(message))
                    .children((failure && !deleted).then(|| {
                        div().id(SharedString::from(retry_selector.clone())).debug_selector(move || retry_selector.clone())
                            .flex_none().px(px(8.0)).py(px(3.0)).rounded(px(4.0))
                            .text_color(rgba(if busy { 0x888888ff } else { 0x248a3dff }))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, cx.listener(move |view, _, _, cx| { view.retry(job.clone(), cx); }))
                            .child(if self.retrying.as_ref() == Some(&row.job.resource_id) { "正在提交…" } else { "重试提取" })
                    }))
            }))
            .children((self.overflow > 0).then(|| div().child(format!("另有 {} 个附件等待提取或需处理。", self.overflow))));
        if !multiple {
            content = content.child(details);
        } else if self.expanded {
            content = content.child(details.max_h(px(96.0)).overflow_y_scroll());
        }
        content.children(self.notice.clone().map(|notice| div().child(notice)))
    }
}
