//! Saving and deleting quicklinks from their forms and action panel. The
//! quicklinks provider lists them; after a change its catalog is loaded again.

use std::{fs, path::Path};

use super::{
    Launcher,
    json_files::{export_message, import_message},
    views::Screen,
};
use gpui::{Context, Window};
use spotlight_core::{
    item_id,
    quicklink::{self, Quicklink},
};
use spotlight_platform_win::links::{self, Browser};

/// The quicklinks provider's id, whose items these are.
const PROVIDER: &str = "quicklinks";
/// What the export file is called unless the user changes it.
const EXPORT_NAME: &str = "Quicklinks.json";

impl Launcher {
    /// Checks the quicklink a form filled in and saves it, then goes back to
    /// root search. A problem is shown on the form instead.
    pub(super) fn save_quicklink(
        &mut self,
        link: Quicklink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.store_quicklink(link) {
            Ok(saved) => {
                self.screens.clear();
                self.enter_screen(window, cx);
                self.reload_quicklinks(window.scale_factor(), cx);
                let message = match &saved.keyword {
                    Some(keyword) => format!("Saved “{}”. Type !{keyword} to open it", saved.name),
                    None => format!("Saved “{}”", saved.name),
                };
                self.show_toast(message.into(), cx);
            }
            Err(why) => {
                if let Some(Screen::Form(form)) = self.screens.last_mut() {
                    form.error = Some(why.into());
                }
                cx.notify();
            }
        }
    }

    /// Normalizes `link`, checks it against the others and writes it.
    fn store_quicklink(&self, link: Quicklink) -> Result<Quicklink, String> {
        let name = link.name.trim().to_owned();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        let target = link.link.trim().to_owned();
        quicklink::check_link(&target)?;
        let keyword = quicklink::normalize_keyword(link.keyword.as_deref().unwrap_or_default())?;
        let open_with = link
            .open_with
            .map(|app| app.trim().to_owned())
            .filter(|app| !app.is_empty());
        let mut link = Quicklink {
            id: link.id,
            name,
            link: target,
            keyword,
            open_with,
        };

        let store = self
            .store
            .as_ref()
            .ok_or("Quicklinks can't be saved: there is no database")?;
        let store = store.lock().map_err(|_| "The database is unavailable")?;
        let others = store.quicklinks().map_err(|err| err.to_string())?;
        if let Some(keyword) = &link.keyword
            && let Some(other) = others
                .iter()
                .find(|other| other.id != link.id && other.keyword.as_ref() == Some(keyword))
        {
            return Err(format!("!{keyword} already opens “{}”", other.name));
        }
        link.id = store
            .save_quicklink(&link)
            .map_err(|err| format!("Couldn't save it: {err}"))?;
        Ok(link)
    }

    /// Deletes a quicklink, with its alias, hotkey and ranking.
    pub(super) fn delete_quicklink(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = item_id(PROVIDER, &id.to_string());
        let title = self
            .search
            .catalog()
            .iter()
            .find(|candidate| candidate.id == item)
            .map(|candidate| candidate.title.clone())
            .unwrap_or_else(|| "Quicklink".into());
        // Right away, not in the background: the catalog is read again below.
        let deleted = self
            .store
            .as_ref()
            .ok_or_else(|| "there is no database".to_owned())
            .and_then(|store| {
                store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())?
                    .delete_quicklink(id)
                    .map_err(|err| err.to_string())
            });
        if let Err(err) = deleted {
            self.show_toast(format!("Couldn't delete “{title}”: {err}").into(), cx);
            return;
        }

        if self.aliases.contains_key(&item) {
            self.set_alias(&item, None, cx);
        }
        if self.item_hotkeys.contains_key(&item) {
            if let Err(err) = self.hotkeys.set_item(&item, None) {
                eprintln!("[spotlight] could not remove the hotkey of {item:?}: {err}");
            }
            self.hotkey_assigned(&item, None, cx);
        }
        if self.frecency.contains_key(&item) {
            self.reset_ranking(&item, cx);
        }
        // Deleted from the Search Quicklinks list: gone from it at once.
        if let Some(Screen::List { view, .. }) = self.screens.last_mut() {
            view.items.retain(|candidate| candidate.id != item);
        }
        let query = self.query(cx);
        self.refilter(&query);
        self.reload_quicklinks(window.scale_factor(), cx);
        self.show_toast(format!("Deleted “{title}”").into(), cx);
    }

    /// Asks for a JSON file (as Raycast exports quicklinks) and adds the
    /// quicklinks in it that aren't there yet.
    pub(super) fn import_quicklinks(&mut self, cx: &mut Context<Self>) {
        self.import_json_file(
            "Import Quicklinks",
            |this, path, cx| match fs::read_to_string(path) {
                Ok(json) => this.import_quicklinks_json(&json, cx),
                Err(err) => format!("Couldn't read it: {err}"),
            },
            cx,
        );
    }

    /// Adds the new quicklinks in `json` (a file's, or a link's) and says
    /// how it went. The app each opens with is matched to an installed
    /// browser by name, or dropped.
    pub(super) fn import_quicklinks_json(&mut self, json: &str, cx: &mut Context<Self>) -> String {
        let browsers = links::browsers();
        let imported = (|| {
            let store = self.store.as_ref().ok_or("There is no database")?;
            let mut store = store.lock().map_err(|_| "The database is unavailable")?;
            let existing = store.quicklinks().map_err(|err| err.to_string())?;
            let mut imported = quicklink::import(json, &existing)?;
            for link in &mut imported.added {
                link.open_with = link
                    .open_with
                    .take()
                    .and_then(|app| browser_exe(&browsers, &app));
            }
            store
                .add_quicklinks(&imported.added)
                .map_err(|err| format!("Couldn't save them: {err}"))?;
            Ok::<_, String>(imported)
        })();
        let imported = match imported {
            Ok(imported) => imported,
            Err(err) => {
                eprintln!("[spotlight] could not import quicklinks: {err}");
                return err;
            }
        };
        self.reload_quicklinks(self.window_scale, cx);
        let skipped = imported.duplicates + imported.taken + imported.invalid;
        if skipped > 0 {
            eprintln!(
                "[spotlight] import skipped {} already there, {} with a keyword in use, {} invalid",
                imported.duplicates, imported.taken, imported.invalid
            );
        }
        import_message("quicklink", imported.added.len(), skipped)
    }

    /// Asks where to save every quicklink as JSON Raycast can import.
    pub(super) fn export_quicklinks(&mut self, cx: &mut Context<Self>) {
        self.export_json_file(
            "Export Quicklinks",
            EXPORT_NAME,
            |this, path| export_message("quicklink", this.export_quicklinks_to(path)),
            cx,
        );
    }

    /// Writes every quicklink to `path` as JSON. Returns how many.
    pub fn export_quicklinks_to(&self, path: &Path) -> Result<usize, String> {
        let saved = self
            .store
            .as_ref()
            .ok_or("There is no database")?
            .lock()
            .map_err(|_| "The database is unavailable")?
            .quicklinks()
            .map_err(|err| err.to_string())?;
        let browsers = links::browsers();
        let json = quicklink::export(&saved, |exe| {
            browsers
                .iter()
                .find(|browser| browser.exe.eq_ignore_ascii_case(exe))
                .map_or_else(|| exe.to_owned(), |browser| browser.name.clone())
        });
        fs::write(path, json).map_err(|err| format!("Couldn't write it: {err}"))?;
        Ok(saved.len())
    }

    /// Quicklinks, and the web search rows, which name a quicklink engine
    /// and carry its link.
    fn reload_quicklinks(&mut self, scale: f32, cx: &mut Context<Self>) {
        self.reload_catalog(PROVIDER, scale, cx);
        self.reload_catalog("web", scale, cx);
        self.reload_catalog("selection", scale, cx);
    }
}

/// The exe of the browser an imported quicklink opens with: `app` is its
/// name (as Raycast writes it) or already an exe of one.
fn browser_exe(browsers: &[Browser], app: &str) -> Option<String> {
    browsers
        .iter()
        .find(|browser| {
            browser.name.eq_ignore_ascii_case(app) || browser.exe.eq_ignore_ascii_case(app)
        })
        .map(|browser| browser.exe.clone())
}
