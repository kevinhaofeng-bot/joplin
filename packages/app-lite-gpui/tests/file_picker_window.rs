#[path = "../src/file_picker/adapter.rs"]
mod file_picker;

use futures::channel::oneshot;
use gpui::{Context, Render, TestAppContext, Window, div};
use std::path::PathBuf;
use std::time::Duration;

struct Empty {
    token: Option<u64>,
}
impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        div()
    }
}

// Removing the window-lifetime binding must leave the real upstream receiver
// alive and make the cancellation assertions fail. No native chooser is mocked.
#[gpui::test]
async fn closing_picker_window_cancels_its_upstream_receiver(cx: &mut TestAppContext) {
    let (_, window_cx) = cx.add_window_view(|_, _| Empty { token: Some(7) });
    let handle = window_cx.update(|window, _| window.window_handle());
    let (sender, receiver) = oneshot::channel::<anyhow::Result<Option<Vec<PathBuf>>>>();
    let result = window_cx.cx.update(|app| file_picker::bind_to_window(app, handle.into(), receiver));
    window_cx.run_until_parked();
    assert!(!sender.is_canceled());
    window_cx.update(|window, _| window.remove_window());
    window_cx.cx.executor().advance_clock(Duration::from_millis(150));
    window_cx.run_until_parked();
    assert!(sender.is_canceled(), "window close must cancel the worker's receiver");
    assert!(result.await.unwrap().is_err());
}

#[gpui::test]
async fn dropping_picker_consumer_cancels_its_upstream_receiver(cx: &mut TestAppContext) {
    let (_, window_cx) = cx.add_window_view(|_, _| Empty { token: Some(7) });
    let handle = window_cx.update(|window, _| window.window_handle());
    let (sender, receiver) = oneshot::channel::<anyhow::Result<Option<Vec<PathBuf>>>>();
    let result = window_cx.cx.update(|app| file_picker::bind_to_window(app, handle.into(), receiver));
    window_cx.run_until_parked();
    drop(result);
    window_cx.cx.executor().advance_clock(Duration::from_millis(150));
    window_cx.run_until_parked();
    assert!(sender.is_canceled());
}

#[gpui::test]
async fn picker_selection_error_and_cancel_are_not_coerced(cx: &mut TestAppContext) {
    let (owner, window_cx) = cx.add_window_view(|_, _| Empty { token: Some(7) });
    let handle = window_cx.update(|window, _| window.window_handle());
    for (index, value) in [Ok(Some(vec![PathBuf::from("/tmp/验收 图片.png")])), Ok(None), Err(anyhow::anyhow!("worker failed"))].into_iter().enumerate() {
        let (sender, receiver) = oneshot::channel();
        let weak_owner = owner.downgrade();
        let result = window_cx.cx.update(|app| file_picker::bind_to_request(
            app, handle.into(), receiver,
            move |app| weak_owner.read_with(app, |owner, _| owner.token == Some(7)).unwrap_or(false),
        ));
        sender.send(value).unwrap();
        let value = result.await.unwrap();
        match index {
            0 => assert_eq!(value.unwrap().unwrap(), vec![PathBuf::from("/tmp/验收 图片.png")]),
            1 => assert_eq!(value.unwrap(), None),
            2 => assert_eq!(value.unwrap_err().to_string(), "worker failed"),
            _ => unreachable!(),
        }
    }
}

// Keeping only the window guard allows a cancelled/replaced request to keep
// its upstream receiver alive. The owner here is a real mounted GPUI entity.
#[gpui::test]
async fn expired_picker_request_cancels_upstream_while_window_remains_open(cx: &mut TestAppContext) {
    for replacement in [None, Some(8)] {
        let (owner, window_cx) = cx.add_window_view(|_, _| Empty { token: Some(7) });
        let handle = window_cx.update(|window, _| window.window_handle());
        let (sender, receiver) = oneshot::channel::<anyhow::Result<Option<Vec<PathBuf>>>>();
        let weak_owner = owner.downgrade();
        let result = window_cx.cx.update(|app| file_picker::bind_to_request(
            app, handle.into(), receiver,
            move |app| weak_owner.read_with(app, |owner, _| owner.token == Some(7)).unwrap_or(false),
        ));
        window_cx.run_until_parked();
        assert!(!sender.is_canceled());
        window_cx.update(|_, app| owner.update(app, |owner, _| owner.token = replacement));
        window_cx.cx.executor().advance_clock(Duration::from_millis(150));
        window_cx.run_until_parked();
        assert!(sender.is_canceled(), "expired request must cancel upstream without closing its window");
        assert!(result.await.unwrap().is_err());
        window_cx.update(|window, _| window.remove_window());
    }
}

#[gpui::test]
async fn expired_request_rejects_an_already_queued_selection(cx: &mut TestAppContext) {
    for already_waiting in [false, true] {
        let (owner, window_cx) = cx.add_window_view(|_, _| Empty { token: Some(7) });
        let handle = window_cx.update(|window, _| window.window_handle());
        let (sender, receiver) = oneshot::channel();
        let weak_owner = owner.downgrade();
        let result = window_cx.cx.update(|app| file_picker::bind_to_request(
            app, handle.into(), receiver,
            move |app| weak_owner.read_with(app, |owner, _| owner.token == Some(7)).unwrap_or(false),
        ));
        if already_waiting {
            window_cx.run_until_parked();
        }
        sender.send(Ok(Some(vec![PathBuf::from("/tmp/stale.png")]))).unwrap();
        window_cx.update(|_, app| owner.update(app, |owner, _| owner.token = None));
        assert!(result.await.unwrap().is_err(), "queued result must not outlive its request");
        window_cx.update(|window, _| window.remove_window());
    }
}
