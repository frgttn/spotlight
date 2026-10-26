//! QR codes in the launcher: the code drawn in its view, copying it as an
//! image or saving it as a PNG file, and scanning the screen or the
//! clipboard's image for codes (see `spotlight_qr`).

use std::{fs, sync::Arc, time::Duration, time::Instant};

use gpui::{Context, RenderImage, Window};
use image::Frame;
use spotlight_core::item::QrSource;
use spotlight_platform_win::{
    clipboard::{self, Content, Image},
    dialog::{self, Request},
    links, screen,
};
use spotlight_qr::Code;

use super::Launcher;

/// How large the code is drawn in its view, in pixels at 100% scale.
pub(super) const QR_SIZE: f32 = 200.;
/// How long the screen is given to repaint without the launcher before
/// it's captured.
const HIDE_SETTLE: Duration = Duration::from_millis(250);

/// The code drawn in the open view: its text, the image, and its size in
/// pixels at 100% scale.
pub(super) struct QrImage {
    text: String,
    image: Arc<RenderImage>,
    size: f32,
}

impl Launcher {
    /// Opens the view of a QR code of `text`, or says why there can't be one.
    pub(super) fn show_qr_code(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        match Code::new(text) {
            Ok(_) => self.push_view(spotlight_qr::view(text, false), window, cx),
            Err(err) => self.show_toast(err.into(), cx),
        }
    }

    /// Puts a QR code of `text` on the clipboard as an image.
    pub(super) fn copy_qr_code(&mut self, text: &str, cx: &mut Context<Self>) {
        let copied = Code::new(text).and_then(|code| {
            let png = code.png()?;
            let size = code.png_size();
            clipboard::write(
                self.hwnd,
                &Content::Image(Image {
                    png,
                    width: size,
                    height: size,
                }),
            )
        });
        self.hide(cx);
        let message = match copied {
            Ok(()) => "Copied QR Code".to_owned(),
            Err(err) => format!("Couldn't copy: {err}"),
        };
        self.show_hud(message.into(), cx);
    }

    /// Asks where to save a QR code of `text` (in Downloads unless the user
    /// goes elsewhere) and saves it there as a PNG file.
    pub(super) fn save_qr_code(&mut self, text: &str, cx: &mut Context<Self>) {
        let png = match Code::new(text).and_then(|code| code.png()) {
            Ok(png) => png,
            Err(err) => return self.show_toast(err.into(), cx),
        };
        self.hide(cx);
        let picked = dialog::save_file(
            Request {
                owner: 0,
                title: "Save QR Code".into(),
                types: vec![("PNG Images", "*.png")],
            },
            links::downloads_folder(),
            "QR Code.png".into(),
        );
        cx.spawn(async move |this, cx| {
            let path = match picked.recv().await {
                Ok(Ok(Some(path))) => path,
                Ok(Err(err)) => {
                    eprintln!("[spotlight] the file dialog failed: {err}");
                    return;
                }
                _ => return,
            };
            let message = match fs::write(&path, &png) {
                Ok(()) => format!(
                    "Saved {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                Err(err) => format!("Couldn't save it: {err}"),
            };
            this.update(cx, |this, cx| this.show_hud(message.into(), cx))
                .ok();
        })
        .detach();
    }

    /// Reads the QR codes on the screen (with the launcher hidden first) or
    /// in the clipboard's image, and opens what they say. Nothing found is
    /// said in the HUD, or in a toast while the launcher is up.
    pub(super) fn scan_qr_codes(
        &mut self,
        source: QrSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source == QrSource::Screen {
            self.hide(cx);
        }
        let owner = self.hwnd;
        cx.spawn_in(window, async move |this, cx| {
            if source == QrSource::Screen {
                cx.background_executor().timer(HIDE_SETTLE).await;
            }
            let scanned =
                cx.background_executor()
                    .spawn(async move {
                        let image = match source {
                            QrSource::Screen => screen::capture()?,
                            QrSource::Clipboard => clipboard::image(owner)?
                                .ok_or("There's no image on the clipboard")?,
                        };
                        Ok::<_, String>(spotlight_qr::read(&image))
                    })
                    .await;
            this.update_in(cx, |this, window, cx| {
                let message = match scanned.as_deref() {
                    Ok(codes) => match spotlight_qr::scanned_view(codes) {
                        Some(view) => {
                            if !this.visible {
                                this.show(Instant::now(), window, cx);
                            }
                            return this.push_view(view, window, cx);
                        }
                        None if source == QrSource::Screen => "No QR codes on the screen",
                        None => "No QR codes in the image",
                    }
                    .to_owned(),
                    Err(err) => err.clone(),
                };
                if this.visible {
                    this.show_toast(message.into(), cx);
                } else {
                    this.show_hud(message.into(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The image of the code `text` for the open view, and its size, made at
    /// the window's scale so its modules stay sharp. One is kept at a time.
    pub(super) fn qr_image(&mut self, text: &str, scale: f32) -> Option<(Arc<RenderImage>, f32)> {
        if let Some(qr) = &self.qr_image
            && qr.text == text
        {
            return Some((qr.image.clone(), qr.size));
        }
        self.forget_qr_image();
        let code = Code::new(text).ok()?;
        let modules = code.full_width() as f32;
        let module = (QR_SIZE * scale / modules).round().max(1.);
        let mut image = code.image(module as u32);
        // GPUI wants BGRA.
        for px in image.pixels_mut() {
            px.0.swap(0, 2);
        }
        let image = Arc::new(RenderImage::new(vec![Frame::new(image)]));
        let size = modules * module / scale;
        self.qr_image = Some(QrImage {
            text: text.to_owned(),
            image: image.clone(),
            size,
        });
        Some((image, size))
    }

    /// Lets go of the code's image; it's freed on the next frame.
    pub(super) fn forget_qr_image(&mut self) {
        if let Some(qr) = self.qr_image.take() {
            self.stale_images.push(qr.image);
        }
    }
}
