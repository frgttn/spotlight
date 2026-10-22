//! The settings window: launcher hotkey, start at login, where data lives.

use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, SharedString, Subscription, Task, Window, div,
    prelude::*, px, rgb, rgba,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_platform_win::{
    autostart,
    hotkey::{self, Hotkey, Recorded, SetError},
    win,
};

/// Logical size of the settings window.
pub const WINDOW_SIZE: (f32, f32) = (560., 330.);

pub enum SettingsEvent {
    /// The launcher hotkey is now this one; persist it and update labels.
    HotkeyChanged(Hotkey),
    /// Start at login was turned on or off here.
    AutostartChanged,
}

pub struct Settings {
    hotkeys: hotkey::Service,
    hotkey: Option<Hotkey>,
    /// Receives what the recorder hook sees; `Some` while recording.
    recording: Option<Task<()>>,
    /// Outcome of the last hotkey change, shown under the row.
    message: Option<(SharedString, bool)>,
    autostart: bool,
    data_path: SharedString,
    recorder: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    pub fn new(
        hotkeys: hotkey::Service,
        hotkey: Option<Hotkey>,
        data_path: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Ok(RawWindowHandle::Win32(handle)) =
            HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        {
            win::dark_title_bar(handle.hwnd.get());
        }
        let recorder = cx.focus_handle();
        // Recording takes every key press system-wide, so it ends as soon as
        // the user clicks elsewhere in the window or switches to another one.
        let subscriptions = [
            cx.on_blur(&recorder, window, |this, _, cx| this.stop_recording(cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.stop_recording(cx);
                }
            }),
        ];
        Self {
            hotkeys,
            hotkey,
            recording: None,
            message: None,
            autostart: autostart::is_enabled(),
            data_path,
            recorder,
            _subscriptions: subscriptions,
        }
    }

    /// Re-reads start at login, which the tray menu can change too.
    pub fn refresh_autostart(&mut self, cx: &mut Context<Self>) {
        self.autostart = autostart::is_enabled();
        cx.notify();
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording.is_some() {
            return;
        }
        window.focus(&self.recorder);
        let (tx, rx) = async_channel::unbounded();
        self.hotkeys.record(tx);
        self.recording = Some(cx.spawn(async move |this, cx| {
            while let Ok(recorded) = rx.recv().await {
                let handled = this.update(cx, |this, cx| this.recorded(recorded, cx));
                if !handled.unwrap_or(true) {
                    break;
                }
            }
        }));
        self.message = None;
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            self.hotkeys.resume();
            cx.notify();
        }
    }

    /// Handles one key press from the recorder. Returns whether to keep recording.
    fn recorded(&mut self, recorded: Recorded, cx: &mut Context<Self>) -> bool {
        if self.recording.is_none() {
            return false;
        }
        let hotkey = match recorded {
            Recorded::Cancel => {
                self.stop_recording(cx);
                return false;
            }
            Recorded::NeedsModifier => {
                self.message = Some(("Add Ctrl, Alt, Shift or Win to the key.".into(), true));
                cx.notify();
                return true;
            }
            Recorded::Hotkey(hotkey) => hotkey,
        };

        // `set` also ends recording, whatever the outcome.
        self.recording = None;
        self.message = Some(match self.hotkeys.set(hotkey) {
            Ok(()) => {
                self.hotkey = Some(hotkey);
                cx.emit(SettingsEvent::HotkeyChanged(hotkey));
                (format!("Spotlight now opens with {hotkey}.").into(), false)
            }
            Err(SetError::InUse) => (
                format!(
                    "{hotkey} is already used by another app or by Windows. Try a different shortcut."
                )
                .into(),
                true,
            ),
            Err(SetError::Other(err)) => (format!("Couldn't use {hotkey}: {err}").into(), true),
        });
        cx.notify();
        false
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = autostart::set_enabled(!self.autostart) {
            eprintln!("[settings] could not change start at login: {err}");
        }
        self.autostart = autostart::is_enabled();
        cx.emit(SettingsEvent::AutostartChanged);
        cx.notify();
    }
}

impl Drop for Settings {
    /// Closing the window mid-recording must not leave the hotkey unregistered.
    fn drop(&mut self) {
        if self.recording.is_some() {
            self.hotkeys.resume();
        }
    }
}

impl Focusable for Settings {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.recorder.clone()
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.recording.is_some();
        let recorder_label: SharedString = if recording {
            "Press a shortcut…".into()
        } else {
            self.hotkey
                .map_or("Not set".into(), |hotkey| hotkey.to_string().into())
        };
        let hint: SharedString = if recording {
            "Esc to cancel".into()
        } else {
            "Click to change".into()
        };

        let hotkey_row = row("Launcher hotkey", "Opens and closes the search window.").child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap_1()
                .child(
                    div()
                        .id("recorder")
                        .track_focus(&self.recorder)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.start_recording(window, cx)),
                        )
                        .min_w(px(180.))
                        .px_3()
                        .py_1p5()
                        .rounded(px(6.))
                        .border_1()
                        .text_center()
                        .cursor_pointer()
                        .when(recording, |el| {
                            el.border_color(rgb(0x4f7cff)).bg(rgba(0x4f7cff26))
                        })
                        .when(!recording, |el| {
                            el.border_color(rgba(0xffffff26))
                                .bg(rgba(0xffffff0d))
                                .hover(|style| style.bg(rgba(0xffffff1a)))
                        })
                        .child(recorder_label),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgba(0xffffff70))
                        .child(hint),
                ),
        );

        let message = self.message.clone().map(|(text, is_error)| {
            div()
                .text_size(px(12.))
                .text_color(if is_error {
                    rgb(0xff8a7a)
                } else {
                    rgb(0x7fd4a0)
                })
                .child(text)
        });

        let autostart_row = row(
            "Start at login",
            "Run Spotlight in the background when you sign in.",
        )
        .child(
            div()
                .id("autostart")
                .on_click(cx.listener(|this, _, _, cx| this.toggle_autostart(cx)))
                .cursor_pointer()
                .w(px(40.))
                .h(px(22.))
                .p(px(3.))
                .rounded_full()
                .flex()
                .when(self.autostart, |el| el.bg(rgb(0x4f7cff)).justify_end())
                .when(!self.autostart, |el| el.bg(rgba(0xffffff26)))
                .child(div().size(px(16.)).rounded_full().bg(rgb(0xffffff))),
        );

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(0x1c1c1e))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .child(div().text_size(px(20.)).child("Settings"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(hotkey_row)
                    .children(message),
            )
            .child(div().h(px(1.)).bg(rgba(0xffffff14)))
            .child(autostart_row)
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgba(0xffffff60))
                    .truncate()
                    .child(format!("Data: {}", self.data_path)),
            )
    }
}

/// A settings row: title and description on the left, the control on the right.
fn row(title: &'static str, description: &'static str) -> gpui::Div {
    div().flex().items_center().justify_between().gap_4().child(
        div().flex().flex_col().gap_0p5().child(title).child(
            div()
                .text_size(px(12.))
                .text_color(rgba(0xffffff80))
                .child(description),
        ),
    )
}
