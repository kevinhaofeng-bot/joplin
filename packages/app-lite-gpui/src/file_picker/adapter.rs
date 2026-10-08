use futures::channel::oneshot;
use futures::FutureExt;
use gpui::{AnyWindowHandle, App};
use std::time::Duration;

/// Bind a chooser to the request which owns it as well as its window.
pub fn bind_to_request<T: 'static>(
    cx: &mut App,
    window: AnyWindowHandle,
    receiver: oneshot::Receiver<anyhow::Result<Option<T>>>,
    is_current: impl Fn(&App) -> bool + 'static,
) -> oneshot::Receiver<anyhow::Result<Option<T>>> {
    let (sender, result) = oneshot::channel();
    cx.spawn(async move |cx| {
        let mut receiver = receiver.fuse();
        loop {
            if sender.is_canceled() {
                return;
            }
            if !cx.update(|app| app.windows().contains(&window) && is_current(app)).unwrap_or(false) {
                let _ = sender.send(Err(anyhow::anyhow!("选择请求已取消或窗口已关闭；内容未改动")));
                return;
            }
            let timer = cx.background_executor().timer(Duration::from_millis(100)).fuse();
            futures::pin_mut!(timer);
            futures::select_biased! {
                selected = receiver => {
                    // The request can expire while awaiting a selection, too.
                    let current = cx.update(|app| app.windows().contains(&window) && is_current(app)).unwrap_or(false);
                    let value = if current {
                        selected.unwrap_or_else(|error| Err(error.into()))
                    } else {
                        Err(anyhow::anyhow!("选择请求已取消或窗口已关闭；内容未改动"))
                    };
                    let _ = sender.send(value);
                    return;
                },
                _ = timer => {},
            }
        }
    }).detach();
    result
}

/// A detached completion task must not keep a chooser alive after its window
/// closes. No view is strongly held by this foreground monitor.
pub fn bind_to_window<T: 'static>(
    cx: &mut App,
    window: AnyWindowHandle,
    receiver: oneshot::Receiver<anyhow::Result<Option<T>>>,
) -> oneshot::Receiver<anyhow::Result<Option<T>>> {
    bind_to_request(cx, window, receiver, |_| true)
}
