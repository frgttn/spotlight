//! Saving and deleting quicklinks from their forms and action panel. The
//! quicklinks provider lists them; after a change its catalog is loaded again.

use super::{Launcher, views::Screen};
use gpui::{Context, Window};
use spotlight_core::{
    item_id,
    quicklink::{self, Quicklink},
};

/// The quicklinks provider's id, whose items these are.
const PROVIDER: &str = "quicklinks";

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
                self.reload_catalog(PROVIDER, window.scale_factor(), cx);
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
        self.reload_catalog(PROVIDER, window.scale_factor(), cx);
        self.show_toast(format!("Deleted “{title}”").into(), cx);
    }
}
