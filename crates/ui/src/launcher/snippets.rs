//! Snippets in the launcher: saving and deleting them from their forms and
//! action panel, pasting one into the app that was active before, and moving
//! them to and from Raycast as JSON. The snippets provider lists them; after a
//! change its view is built again.

use std::{fs, path::Path};

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use spotlight_core::{
    clip::IgnoredApp,
    frecency, item_id,
    snippet::{self, EXCLUDED_APPS_SETTING, Inputs, Snippet},
};
use spotlight_platform_win::{
    clipboard::{self, Content},
    expander::{self, Ask, Keyword},
    links, win,
};

use super::{
    Launcher,
    json_files::{export_message, import_message},
    views::Screen,
};

/// The snippets provider's id, whose items these are.
pub(super) const PROVIDER: &str = "snippets";
/// What the export file is called unless the user changes it.
const EXPORT_NAME: &str = "Snippets.json";
/// How long a snippet's form gets to come up and take the keyboard before
/// the keys typed after its keyword are sent to it.
const FORM_SETTLE: Duration = Duration::from_millis(80);

impl Launcher {
    /// Starts expanding snippet keywords typed in other apps. The self-test
    /// doesn't: it runs beside a normal instance, which already does.
    pub fn start_expander(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (asks_tx, asks_rx) = async_channel::unbounded();
        match expander::spawn(self.hwnd, asks_tx) {
            Ok(service) => {
                self.expander = Some(service);
                self.set_excluded_snippet_apps(&self.excluded_snippet_apps());
                self.sync_expander();
            }
            Err(err) => {
                eprintln!("[spotlight] snippet keywords won't expand: {err}");
                return;
            }
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(ask) = asks_rx.recv().await {
                this.update_in(cx, |this, window, cx| {
                    this.ask_snippet_arguments(ask, window, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// A keyword whose snippet has arguments was typed in another app: shows
    /// the snippet's form, which pastes it there (see [`Self::paste_snippet`]).
    fn ask_snippet_arguments(&mut self, ask: Ask, window: &mut Window, cx: &mut Context<Self>) {
        if !(self.visible && win::is_visible(self.hwnd)) {
            self.show(Instant::now(), window, cx);
        }
        // Not opened by its hotkey, so Windows may not hand it the foreground.
        win::take_foreground(self.hwnd);
        self.previous_window = ask.window;
        self.screens.clear();
        self.push_view(
            snippet::arguments_form(&ask.name, &ask.text, true),
            window,
            cx,
        );
        self.keyword_target = Some(ask.window);
        let expander = self.expander.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(FORM_SETTLE).await;
            if let Some(expander) = expander {
                expander.form_shown();
            }
        })
        .detach();
    }

    /// Turns expanding keywords typed in other apps on or off (Settings).
    pub fn set_snippet_expansion(&mut self, enabled: bool) {
        self.snippet_expansion = enabled;
        self.sync_expander();
    }

    /// The apps where keywords don't expand, as saved. For Settings.
    pub fn excluded_snippet_apps(&self) -> Vec<IgnoredApp> {
        self.store
            .as_ref()
            .and_then(|store| store.lock().ok()?.setting(EXCLUDED_APPS_SETTING).ok()?)
            .map(|text| IgnoredApp::parse_list(&text))
            .unwrap_or_default()
    }

    /// Keywords stop expanding in `apps` (Settings saves the list itself).
    pub fn set_excluded_snippet_apps(&self, apps: &[IgnoredApp]) {
        if let Some(expander) = &self.expander {
            expander.set_excluded(apps.iter().map(|app| app.path.clone()).collect());
        }
    }

    /// Tells the expander which keywords to look for: every snippet's, or
    /// none while Snippets or expanding is turned off in Settings.
    pub(super) fn sync_expander(&self) {
        let Some(expander) = &self.expander else {
            return;
        };
        let enabled = self.snippet_expansion
            && self
                .search
                .providers()
                .iter()
                .position(|provider| provider.id() == PROVIDER)
                .is_some_and(|ix| self.search.is_enabled(ix));
        let snippets = if enabled {
            self.store
                .as_ref()
                .and_then(|store| store.lock().ok()?.snippets().ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        expander.set_snippets(
            snippets
                .iter()
                .filter_map(|snippet| {
                    Some(Keyword {
                        keyword: snippet.keyword.clone()?,
                        name: snippet.name.clone(),
                        text: snippet::inline(&snippet.text, &snippets),
                    })
                })
                .collect(),
        );
    }

    /// Checks the snippet a form filled in and saves it, then goes back to
    /// Search Snippets if the form came from there, otherwise to root search.
    /// A problem is shown on the form instead.
    pub(super) fn save_snippet(
        &mut self,
        snippet: Snippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.store_snippet(snippet) {
            Ok(saved) => {
                self.screens.pop();
                let from_list = matches!(
                    self.screens.last(),
                    Some(Screen::List { origin: Some((provider, _)), .. }) if provider == PROVIDER
                );
                if !from_list {
                    self.screens.clear();
                }
                self.enter_screen(window, cx);
                let item = item_id(PROVIDER, &saved.id.to_string());
                self.refresh_screen(Some(&item), window, cx);
                self.sync_expander();
                let message = match &saved.keyword {
                    Some(keyword) => {
                        format!(
                            "Saved “{}”. Type {keyword} anywhere to paste it",
                            saved.name
                        )
                    }
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

    /// Normalizes `snippet`, checks it against the others and writes it.
    fn store_snippet(&self, snippet: Snippet) -> Result<Snippet, String> {
        let name = snippet.name.trim().to_owned();
        let text = snippet::normalize_text(&snippet.text);
        snippet::check(&name, &text)?;
        let keyword = snippet::normalize_keyword(snippet.keyword.as_deref().unwrap_or_default())?;
        let mut snippet = Snippet {
            id: snippet.id,
            name,
            text,
            keyword,
        };

        let store = self
            .store
            .as_ref()
            .ok_or("Snippets can't be saved: there is no database")?;
        let store = store.lock().map_err(|_| "The database is unavailable")?;
        let others = store.snippets().map_err(|err| err.to_string())?;
        if let Some(keyword) = &snippet.keyword
            && let Some(other) = others
                .iter()
                .find(|other| other.id != snippet.id && other.keyword.as_ref() == Some(keyword))
        {
            return Err(format!("{keyword} already pastes “{}”", other.name));
        }
        snippet.id = store
            .save_snippet(&snippet, frecency::now())
            .map_err(|err| format!("Couldn't save it: {err}"))?;
        Ok(snippet)
    }

    pub(super) fn delete_snippet(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let item = item_id(PROVIDER, &id.to_string());
        let title = self
            .list_items()
            .and_then(|items| items.iter().find(|candidate| candidate.id == item))
            .map_or_else(|| "Snippet".to_owned(), |candidate| candidate.title.clone());
        let deleted = self
            .store
            .as_ref()
            .ok_or_else(|| "there is no database".to_owned())
            .and_then(|store| {
                store
                    .lock()
                    .map_err(|_| "store lock poisoned".to_owned())?
                    .delete_snippet(id)
                    .map_err(|err| err.to_string())
            });
        if let Err(err) = deleted {
            self.show_toast(format!("Couldn't delete “{title}”: {err}").into(), cx);
            return;
        }
        self.refresh_screen(None, window, cx);
        self.sync_expander();
        self.show_toast(format!("Deleted “{title}”").into(), cx);
    }

    /// Fills in `text`'s placeholders, puts it on the clipboard and, with
    /// `paste`, pastes it into the window that was active before the
    /// launcher, the caret at `{cursor}`. A snippet whose keyword was typed
    /// goes back to the expander, which puts the clipboard back after.
    pub(super) fn paste_snippet(&mut self, text: &str, paste: bool, cx: &mut Context<Self>) {
        if let Some(target) = self.keyword_target.take()
            && paste
            && let Some(expander) = self.expander.clone()
        {
            // While the launcher still has the foreground to hand over.
            clipboard::bring_to_front(target);
            self.hide(cx);
            expander.paste(target, text.to_owned());
            return;
        }
        let copied = snippet::needs_clipboard(text)
            .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
            .flatten();
        let moment = links::moment();
        let expansion = snippet::expand(
            text,
            &Inputs {
                moment: &moment,
                clipboard: copied.as_deref(),
                values: &[],
            },
        );
        // Counted in graphemes, so the same with `\n` or `\r\n` line breaks.
        let caret_back = expansion.moves_back();
        let text = snippet::normalize_text(&expansion.text).replace('\n', "\r\n");
        if let Err(err) = clipboard::write(self.hwnd, &Content::Text(text)) {
            eprintln!("[spotlight] could not copy the snippet: {err}");
            self.hide(cx);
            self.show_hud(format!("Couldn't copy: {err}").into(), cx);
            return;
        }
        // While the launcher still has the foreground, which lets it hand
        // the foreground back.
        let pasted = paste && clipboard::paste_into(self.previous_window, caret_back);
        self.hide(cx);
        if !pasted {
            self.show_hud("Copied to Clipboard".into(), cx);
        }
    }

    /// Asks for a JSON file (as Raycast exports snippets) and adds the
    /// snippets in it that aren't there yet.
    pub(super) fn import_snippets(&mut self, cx: &mut Context<Self>) {
        self.import_json_file(
            "Import Snippets",
            |this, path, _| this.import_snippets_from(path),
            cx,
        );
    }

    /// Adds the new snippets in the JSON file at `path` and says how it went.
    /// Public for the self-test, which skips the dialog.
    pub fn import_snippets_from(&mut self, path: &Path) -> String {
        match fs::read_to_string(path) {
            Ok(json) => self.import_snippets_json(&json),
            Err(err) => {
                eprintln!("[spotlight] could not read {}: {err}", path.display());
                format!("Couldn't read it: {err}")
            }
        }
    }

    /// Adds the new snippets in `json` (a file's, or a link's) and says how
    /// it went.
    pub(super) fn import_snippets_json(&mut self, json: &str) -> String {
        let imported = (|| {
            let store = self.store.as_ref().ok_or("There is no database")?;
            let mut store = store.lock().map_err(|_| "The database is unavailable")?;
            let existing = store.snippets().map_err(|err| err.to_string())?;
            let imported = snippet::import(json, &existing)?;
            store
                .add_snippets(&imported.added, frecency::now())
                .map_err(|err| format!("Couldn't save them: {err}"))?;
            Ok::<_, String>(imported)
        })();
        let imported = match imported {
            Ok(imported) => imported,
            Err(err) => {
                eprintln!("[spotlight] could not import snippets: {err}");
                return err;
            }
        };
        self.sync_expander();
        let skipped = imported.duplicates + imported.taken + imported.invalid;
        if skipped > 0 {
            eprintln!(
                "[spotlight] import skipped {} already there, {} with a keyword in use, {} invalid",
                imported.duplicates, imported.taken, imported.invalid
            );
        }
        import_message("snippet", imported.added.len(), skipped)
    }

    /// Asks where to save every snippet as JSON Raycast can import.
    pub(super) fn export_snippets(&mut self, cx: &mut Context<Self>) {
        self.export_json_file(
            "Export Snippets",
            EXPORT_NAME,
            |this, path| export_message("snippet", this.export_snippets_to(path)),
            cx,
        );
    }

    /// Writes every snippet to `path` as JSON. Returns how many. Public for
    /// the self-test, which skips the dialog.
    pub fn export_snippets_to(&self, path: &Path) -> Result<usize, String> {
        let snippets = self
            .store
            .as_ref()
            .ok_or("There is no database")?
            .lock()
            .map_err(|_| "The database is unavailable")?
            .snippets()
            .map_err(|err| err.to_string())?;
        fs::write(path, snippet::export(&snippets))
            .map_err(|err| format!("Couldn't write it: {err}"))?;
        Ok(snippets.len())
    }
}
