//! Clipboard history in the launcher: putting an entry back on the clipboard
//! (and pasting it into the app that was active before), pinning and
//! deleting entries, and the preview beside the list. The clipboard provider
//! lists the entries; after a change its view is built again.

use std::{fs, sync::Arc};

use gpui::{
    AnyElement, Context, ObjectFit, RenderImage, SharedString, Window, div, img, prelude::*, px,
    rgba,
};
use image::{Frame, ImageFormat};
use spotlight_core::{
    clip::{ClipKind, IGNORED_APPS_SETTING, IgnoredApp},
    item::SearchItem,
    view::{Preview, PreviewBody},
};
use spotlight_platform_win::clipboard::{self, Content, Image};
use spotlight_store::Clip;

use super::{Launcher, views::Screen};
use crate::theme;

/// Width of the list when a preview is beside it.
pub(super) const PREVIEW_LIST_WIDTH: f32 = 290.;
/// A file list in the preview stops after this many.
const PREVIEW_FILES: usize = 50;

/// The preview's image, decoded off the UI thread.
pub(super) enum PreviewImage {
    Loading(String),
    Ready(String, Arc<RenderImage>),
    Failed(String),
}

impl PreviewImage {
    fn path(&self) -> &str {
        match self {
            PreviewImage::Loading(path)
            | PreviewImage::Ready(path, _)
            | PreviewImage::Failed(path) => path,
        }
    }
}

impl Launcher {
    /// Puts entry `id` back on the clipboard; with `paste`, also pastes it
    /// into the window that was active before the launcher opened. Hides the
    /// launcher either way.
    pub(super) fn restore_clip(&mut self, id: i64, paste: bool, cx: &mut Context<Self>) {
        let clip = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok()?.clip(id).ok().flatten());
        let content = clip.as_ref().and_then(content_of);
        let written = match &content {
            Some(content) => clipboard::write(self.hwnd, content),
            None => Err("This entry is gone".into()),
        };
        if let Err(err) = written {
            eprintln!("[spotlight] could not restore clipboard entry {id}: {err}");
            self.hide(cx);
            self.show_hud(format!("Couldn't copy: {err}").into(), cx);
            return;
        }
        // While the launcher still has the foreground, which lets it hand
        // the foreground back.
        let pasted = paste && clipboard::paste_into(self.previous_window, 0);
        self.hide(cx);
        if !pasted {
            self.show_hud("Copied to Clipboard".into(), cx);
        }
    }

    pub(super) fn pin_clip(
        &mut self,
        id: i64,
        pinned: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| store.pin_clip(id, pinned));
        if let Some(Err(err)) = saved {
            eprintln!("[spotlight] could not pin clipboard entry {id}: {err}");
        }
        // Pinning moves the entry; it stays selected.
        let item_id = spotlight_core::item_id(PROVIDER, &id.to_string());
        self.refresh_screen(Some(&item_id), window, cx);
        self.show_toast(if pinned { "Pinned" } else { "Unpinned" }.into(), cx);
    }

    pub(super) fn delete_clip(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let deleted = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| store.delete_clip(id));
        match deleted {
            Some(Ok(Some(clip))) => clip.remove_files(),
            Some(Err(err)) => eprintln!("[spotlight] could not delete clipboard entry {id}: {err}"),
            _ => {}
        }
        self.refresh_screen(None, window, cx);
        self.show_toast("Entry deleted".into(), cx);
    }

    pub(super) fn clear_clips(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cleared = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|mut store| store.clear_clips());
        let count = match cleared {
            Some(Ok(removed)) => {
                removed.iter().for_each(Clip::remove_files);
                removed.len()
            }
            Some(Err(err)) => {
                eprintln!("[spotlight] could not clear the clipboard history: {err}");
                0
            }
            None => 0,
        };
        self.refresh_screen(None, window, cx);
        let message = match count {
            1 => "Deleted 1 entry".to_owned(),
            count => format!("Deleted {count} entries"),
        };
        self.show_toast(message.into(), cx);
    }

    /// Stops keeping copies from `app` (the recorder reads the list from the
    /// settings on every copy).
    pub(super) fn ignore_clip_app(&mut self, app: IgnoredApp, cx: &mut Context<Self>) {
        let saved = self
            .store
            .as_ref()
            .ok_or_else(|| "there is no database".to_owned())
            .and_then(|store| {
                let store = store.lock().map_err(|_| "store lock poisoned".to_owned())?;
                let mut apps = ignored_apps(&store);
                if !IgnoredApp::contains(&apps, &app.path) {
                    apps.push(app.clone());
                }
                store
                    .set_setting(IGNORED_APPS_SETTING, &IgnoredApp::format_list(&apps))
                    .map_err(|err| err.to_string())
            });
        let message = match saved {
            Ok(()) => format!("Copies from {} won't be kept", app.name),
            Err(err) => {
                eprintln!("[spotlight] could not ignore {}: {err}", app.path);
                format!("Couldn't ignore {}", app.name)
            }
        };
        self.show_toast(message.into(), cx);
    }

    /// The apps whose copies aren't kept, for Settings.
    pub fn ignored_clip_apps(&self) -> Vec<IgnoredApp> {
        self.store
            .as_ref()
            .and_then(|store| store.lock().ok())
            .map(|store| ignored_apps(&store))
            .unwrap_or_default()
    }

    /// Whether the open list shows a preview beside its rows.
    pub(super) fn shows_previews(&self) -> bool {
        matches!(
            self.screens.last(),
            Some(Screen::List { view, grid: false, .. })
                if view.items.iter().any(|item| item.preview.is_some())
        )
    }

    /// The selected row's preview: its text, image or files, and the facts
    /// under them.
    pub(super) fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let item: Option<SearchItem> = self
            .results
            .get(self.selected)
            .map(|row| self.item(row).clone());
        let pane = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .border_l_1()
            .border_color(rgba(theme::HAIRLINE))
            .flex()
            .flex_col();
        let Some((item, preview)) = item.and_then(|item| {
            let preview = item.preview.clone()?;
            Some((item, preview))
        }) else {
            return pane.into_any_element();
        };
        let Preview { body, metadata } = *preview;
        let body: AnyElement = match body {
            PreviewBody::Text(text) => div()
                .id(SharedString::from(format!("preview:{}", item.id)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_4()
                .py_3()
                .font_family("Consolas")
                .text_size(px(12.5))
                .line_height(px(18.))
                .text_color(rgba(theme::TEXT))
                .child(SharedString::from(text))
                .into_any_element(),
            PreviewBody::Image(path) => {
                let content: AnyElement = match self.preview_image(&path, cx) {
                    Some(image) => img(image)
                        .size_full()
                        .object_fit(ObjectFit::ScaleDown)
                        .into_any_element(),
                    None if matches!(self.preview_image, Some(PreviewImage::Failed(_))) => div()
                        .text_size(px(12.))
                        .text_color(rgba(theme::TEXT_FAINT))
                        .child("The image file is missing")
                        .into_any_element(),
                    None => div().into_any_element(),
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(content)
                    .into_any_element()
            }
            PreviewBody::Files(paths) => {
                let more = paths.len().saturating_sub(PREVIEW_FILES);
                div()
                    .id(SharedString::from(format!("preview:{}", item.id)))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(paths.into_iter().take(PREVIEW_FILES).map(|path| {
                        let (folder, name) = path
                            .trim_end_matches(['\\', '/'])
                            .rsplit_once(['\\', '/'])
                            .map(|(folder, name)| (folder.to_owned(), name.to_owned()))
                            .unwrap_or((String::new(), path.clone()));
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_size(px(13.)).truncate().child(name))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgba(theme::TEXT_FAINT))
                                    .truncate()
                                    .child(folder),
                            )
                    }))
                    .when(more > 0, |list| {
                        list.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgba(theme::TEXT_FAINT))
                                .child(format!("and {more} more")),
                        )
                    })
                    .into_any_element()
            }
        };
        let facts = div()
            .flex_none()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(rgba(theme::HAIRLINE))
            .flex()
            .flex_col()
            .gap_1()
            .children(metadata.into_iter().map(|(label, value)| {
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgba(theme::TEXT_FAINT))
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(rgba(theme::TEXT_MUTED))
                            .child(value),
                    )
            }));
        pane.child(body).child(facts).into_any_element()
    }

    /// The decoded image at `path`, or `None` while it loads (then the view
    /// redraws) or when it can't be read. One image is kept at a time; the
    /// one it replaces is freed on the next frame.
    fn preview_image(&mut self, path: &str, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        match &self.preview_image {
            Some(PreviewImage::Ready(loaded, image)) if loaded == path => {
                return Some(image.clone());
            }
            Some(current) if current.path() == path => return None,
            _ => {}
        }
        self.forget_preview_image();
        self.preview_image = Some(PreviewImage::Loading(path.to_owned()));
        let path = path.to_owned();
        let load = cx.background_executor().spawn({
            let path = path.clone();
            async move { decode(&path) }
        });
        cx.spawn(async move |this, cx| {
            let image = load.await;
            this.update(cx, |this, cx| {
                let current = matches!(&this.preview_image, Some(PreviewImage::Loading(loading)) if *loading == path);
                match (current, image) {
                    (true, Some(image)) => this.preview_image = Some(PreviewImage::Ready(path, image)),
                    (true, None) => this.preview_image = Some(PreviewImage::Failed(path)),
                    (false, Some(image)) => this.stale_images.push(image),
                    (false, None) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        None
    }

    /// Lets go of the preview image; it's freed on the next frame.
    pub(super) fn forget_preview_image(&mut self) {
        if let Some(PreviewImage::Ready(_, image)) = self.preview_image.take() {
            self.stale_images.push(image);
        }
    }

    /// Frees images no longer shown: GPUI keeps an image's texture until told.
    pub(super) fn drop_stale_images(&mut self, window: &mut Window) {
        for image in self.stale_images.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

/// The clipboard provider's id, whose view lists the entries.
const PROVIDER: &str = "clipboard";

fn ignored_apps(store: &spotlight_store::Store) -> Vec<IgnoredApp> {
    store
        .setting(IGNORED_APPS_SETTING)
        .ok()
        .flatten()
        .map(|text| IgnoredApp::parse_list(&text))
        .unwrap_or_default()
}

/// What to put back on the clipboard for `clip`.
fn content_of(clip: &Clip) -> Option<Content> {
    Some(match clip.kind {
        ClipKind::Text => Content::Text(clip.content.clone()),
        ClipKind::Files => Content::Files(clip.content.lines().map(str::to_owned).collect()),
        ClipKind::Image => Content::Image(Image {
            png: fs::read(&clip.content).ok()?,
            width: clip.width.unwrap_or_default(),
            height: clip.height.unwrap_or_default(),
        }),
    })
}

/// Decodes a PNG for GPUI, which wants BGRA.
fn decode(path: &str) -> Option<Arc<RenderImage>> {
    let bytes = fs::read(path).ok()?;
    let mut image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .ok()?
        .into_rgba8();
    for px in image.pixels_mut() {
        px.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![Frame::new(image)])))
}
