//! Asking for a JSON file to import or a place to export one, with Windows'
//! own dialogs so they offer only JSON files. The launcher hides first, so a
//! dialog doesn't belong to a window that hides when it loses the focus.

use std::path::{Path, PathBuf};

use gpui::Context;
use spotlight_platform_win::{
    dialog::{self, FileType, Request},
    links,
};

use super::Launcher;

const JSON: FileType = ("JSON Files", "*.json");
const ANY: FileType = ("All Files", "*.*");

impl Launcher {
    /// Asks for a JSON file, then hands it to `import`, whose message goes
    /// in the HUD.
    pub(super) fn import_json_file(
        &mut self,
        title: &str,
        import: impl FnOnce(&mut Self, &Path, &mut Context<Self>) -> String + 'static,
        cx: &mut Context<Self>,
    ) {
        self.hide(cx);
        let picked = dialog::open_file(Request {
            owner: 0,
            title: title.to_owned(),
            types: vec![JSON, ANY],
        });
        cx.spawn(async move |this, cx| {
            let Some(path) = picked_path(picked.recv().await) else {
                return;
            };
            this.update(cx, |this, cx| {
                let message = import(this, &path, cx);
                this.show_hud(message.into(), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Asks where to save `name` (in Downloads unless the user goes
    /// elsewhere), then hands the path to `export`, whose message goes in
    /// the HUD.
    pub(super) fn export_json_file(
        &mut self,
        title: &str,
        name: &str,
        export: impl FnOnce(&Self, &Path) -> String + 'static,
        cx: &mut Context<Self>,
    ) {
        self.hide(cx);
        let picked = dialog::save_file(
            Request {
                owner: 0,
                title: title.to_owned(),
                types: vec![JSON],
            },
            links::downloads_folder(),
            name.to_owned(),
        );
        cx.spawn(async move |this, cx| {
            let Some(path) = picked_path(picked.recv().await) else {
                return;
            };
            this.update(cx, |this, cx| {
                let message = export(this, &path);
                this.show_hud(message.into(), cx);
            })
            .ok();
        })
        .detach();
    }
}

fn picked_path(
    answer: Result<Result<Option<PathBuf>, String>, async_channel::RecvError>,
) -> Option<PathBuf> {
    match answer {
        Ok(Ok(path)) => path,
        Ok(Err(err)) => {
            eprintln!("[spotlight] the file dialog failed: {err}");
            None
        }
        Err(_) => None,
    }
}

/// How an import went, for the HUD: `what` is "snippet" or "quicklink".
pub(super) fn import_message(what: &str, added: usize, skipped: usize) -> String {
    match (added, skipped) {
        (0, 0) => format!("No {what}s in that file"),
        (0, _) => "Nothing new to import".into(),
        (1, 0) => format!("Imported 1 {what}"),
        (added, 0) => format!("Imported {added} {what}s"),
        (added, skipped) => format!("Imported {added}, skipped {skipped}"),
    }
}

/// How an export went, for the HUD.
pub(super) fn export_message(what: &str, exported: Result<usize, String>) -> String {
    match exported {
        Ok(1) => format!("Exported 1 {what}"),
        Ok(count) => format!("Exported {count} {what}s"),
        Err(err) => err,
    }
}
