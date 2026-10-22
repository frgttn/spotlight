//! The settings window: launcher hotkey, smooth scrolling, start at login.
//!
//! The window has no system frame. The header is the drag area and carries
//! its own close button, so it matches the launcher instead of a stock
//! Windows dialog.

use gpui::{
    App, ClickEvent, Context, ElementId, EventEmitter, FocusHandle, Focusable, KeyBinding,
    SharedString, Stateful, Subscription, Task, Window, WindowControlArea, actions, div,
    prelude::*, px, rgba,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use spotlight_platform_win::{
    autostart,
    hotkey::{self, Hotkey, Recorded, SetError},
    win,
};

use crate::theme;

actions!(settings, [Close]);

const CONTEXT: &str = "Settings";
/// Logical size of the settings window.
pub const WINDOW_SIZE: (f32, f32) = (600., 376.);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Close, Some(CONTEXT))]);
}

pub enum SettingsEvent {
    /// The launcher hotkey is now this one; persist it and update labels.
    HotkeyChanged(Hotkey),
    SmoothScrollingChanged(bool),
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
    smooth_scrolling: bool,
    autostart: bool,
    data_path: SharedString,
    focus: FocusHandle,
    recorder: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<SettingsEvent> for Settings {}

impl Settings {
    pub fn new(
        hotkeys: hotkey::Service,
        hotkey: Option<Hotkey>,
        smooth_scrolling: bool,
        data_path: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Ok(RawWindowHandle::Win32(handle)) =
            HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        {
            win::round_corners(handle.hwnd.get());
        }
        // No title bar draws it, but the taskbar and Alt+Tab still show it.
        window.set_window_title("Spotlight Settings");
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
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            hotkeys,
            hotkey,
            recording: None,
            message: None,
            smooth_scrolling,
            autostart: autostart::is_enabled(),
            data_path,
            focus,
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

    fn toggle_smooth_scrolling(&mut self, cx: &mut Context<Self>) {
        self.smooth_scrolling = !self.smooth_scrolling;
        cx.emit(SettingsEvent::SmoothScrollingChanged(self.smooth_scrolling));
        cx.notify();
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = autostart::set_enabled(!self.autostart) {
            eprintln!("[settings] could not change start at login: {err}");
        }
        self.autostart = autostart::is_enabled();
        cx.emit(SettingsEvent::AutostartChanged);
        cx.notify();
    }

    fn close(&mut self, _: &Close, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .h(px(48.))
            .pl_5()
            .pr_2()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgba(theme::HAIRLINE))
            .child(
                // Dragging the window works anywhere in the header except the close button.
                div()
                    .id("drag")
                    .window_control_area(WindowControlArea::Drag)
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(18.))
                            .rounded(px(5.))
                            .bg(rgba(theme::ACCENT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .child("S"),
                    )
                    .child(div().text_size(px(14.)).child("Spotlight Settings")),
            )
            .child(
                div()
                    .id("close")
                    .size(px(32.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgba(theme::TEXT_MUTED))
                    .hover(|style| {
                        style
                            .bg(rgba(theme::SURFACE_HOVER))
                            .text_color(rgba(theme::TEXT))
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| this.close(&Close, window, cx)))
                    .child("✕"),
            )
    }

    fn hotkey_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.recording.is_some();
        let content: gpui::AnyElement = if recording {
            div()
                .text_color(rgba(theme::ACCENT))
                .child("Press a shortcut…")
                .into_any_element()
        } else if let Some(hotkey) = self.hotkey {
            div()
                .flex()
                .gap_1()
                .children(
                    hotkey
                        .to_string()
                        .split('+')
                        .map(|part| keycap(part.to_owned())),
                )
                .into_any_element()
        } else {
            div()
                .text_color(rgba(theme::TEXT_MUTED))
                .child("Record shortcut")
                .into_any_element()
        };

        div()
            .id("recorder")
            .track_focus(&self.recorder)
            .on_click(cx.listener(|this, _, window, cx| this.start_recording(window, cx)))
            .min_w(px(150.))
            .h(px(34.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .cursor_pointer()
            .when(recording, |el| {
                el.border_color(rgba(theme::ACCENT))
                    .bg(rgba(theme::ACCENT_SOFT))
            })
            .when(!recording, |el| {
                el.border_color(rgba(theme::SURFACE_BORDER))
                    .hover(|style| style.bg(rgba(theme::SURFACE_HOVER)))
            })
            .child(content)
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
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hotkey_hint = if self.recording.is_some() {
            "Press the keys now, or Esc to cancel."
        } else {
            "Opens and closes the search window. Click to change."
        };
        let message = self.message.clone().map(|(text, is_error)| {
            div()
                .px_4()
                .pb_3()
                .text_size(px(12.))
                .text_color(rgba(if is_error {
                    theme::ERROR
                } else {
                    theme::SUCCESS
                }))
                .child(text)
        });

        let launcher = card()
            .child(row("Launcher hotkey", hotkey_hint).child(self.hotkey_control(cx)))
            .children(message)
            .child(divider())
            .child(
                row(
                    "Smooth scrolling",
                    "Animate the result list when you scroll or move the selection.",
                )
                .child(switch(
                    "smooth-scrolling",
                    self.smooth_scrolling,
                    cx.listener(|this, _, _, cx| this.toggle_smooth_scrolling(cx)),
                )),
            );

        let system = card().child(
            row(
                "Start at login",
                "Run Spotlight in the background when you sign in.",
            )
            .child(switch(
                "autostart",
                self.autostart,
                cx.listener(|this, _, _, cx| this.toggle_autostart(cx)),
            )),
        );

        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::close))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(theme::BACKGROUND_TINT))
            .text_color(rgba(theme::TEXT))
            .text_size(px(14.))
            .child(self.header(cx))
            .child(
                // Focus lives below the header: an element that tracks focus
                // claims mouse-downs on it, and a claimed mouse-down in the
                // header would stop Windows from starting a window drag.
                div()
                    .track_focus(&self.focus)
                    .flex_1()
                    .px_5()
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_label("Launcher"))
                    .child(launcher)
                    .child(div().h(px(6.)))
                    .child(section_label("System"))
                    .child(system),
            )
            .child(
                div()
                    .flex_none()
                    .px_5()
                    .py_3()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_size(px(11.))
                    .text_color(rgba(theme::TEXT_FAINT))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("Data: {}", self.data_path)),
                    )
                    .child(div().flex_none().child("Esc to close")),
            )
    }
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .px_1()
        .text_size(px(11.))
        .text_color(rgba(theme::TEXT_FAINT))
        .child(text.to_uppercase())
}

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(rgba(theme::SURFACE_BORDER))
        .bg(rgba(theme::SURFACE))
}

fn divider() -> impl IntoElement {
    div().mx_4().h(px(1.)).bg(rgba(theme::HAIRLINE))
}

/// A settings row: title and description on the left, the control on the right.
fn row(title: &'static str, description: &'static str) -> gpui::Div {
    div()
        .min_h(px(60.))
        .px_4()
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(title)
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_MUTED))
                        .child(description),
                ),
        )
}

fn keycap(label: String) -> impl IntoElement {
    div()
        .min_w(px(24.))
        .h(px(22.))
        .px_1p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .bg(rgba(theme::SURFACE_HOVER))
        .border_b_1()
        .border_color(rgba(0x00000066))
        .text_size(px(12.))
        .child(label)
}

fn switch(
    id: impl Into<ElementId>,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .on_click(on_click)
        .flex_none()
        .cursor_pointer()
        .w(px(40.))
        .h(px(22.))
        .p(px(3.))
        .rounded_full()
        .flex()
        .when(on, |el| el.bg(rgba(theme::ACCENT)).justify_end())
        .when(!on, |el| el.bg(rgba(theme::SURFACE_HOVER)))
        .child(div().size(px(16.)).rounded_full().bg(rgba(0xffffffff)))
}
