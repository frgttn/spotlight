//! Memory floor of GPUI itself: one window with a text label, nothing else.
//! Prints RAM after 5 s with the window shown, then after 5 s hidden, then quits.

use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_platform_win::{metrics, win};

struct Baseline;

impl Render for Baseline {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x1c1c1e))
            .text_color(rgb(0xececec))
            .child("baseline")
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.), px(460.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Baseline),
            )
            .unwrap();
        let hwnd = window
            .update(cx, |_, window, _| match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
                _ => 0,
            })
            .unwrap();

        cx.spawn(async move |cx| {
            let executor = cx.background_executor().clone();
            executor.timer(Duration::from_secs(5)).await;
            let shown = metrics::memory();
            win::hide(hwnd);
            executor.timer(Duration::from_secs(5)).await;
            let hidden = metrics::memory();
            eprintln!(
                "[baseline] shown: ws {:.1} MB, private {:.1} MB | hidden: ws {:.1} MB, private {:.1} MB",
                shown.working_set_mb, shown.private_mb, hidden.working_set_mb, hidden.private_mb
            );
            cx.update(|cx| cx.quit()).ok();
        })
        .detach();
    });
}
