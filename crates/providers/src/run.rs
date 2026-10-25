//! Run: commands as Windows' Run dialog (Win+R) takes them. After the prefix
//! (`>` unless changed in Settings) root search shows only Run: the command
//! typed, then the ones run before that match it. Without the prefix, a
//! command's name (`gpedit`) gets a row to run it, as the Start menu finds it,
//! under the other results.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use spotlight_core::{
    frecency,
    item::{Command, Icon, SearchItem, shortcut},
    item_id,
    provider::Provider,
    run::{self, HISTORY_SHOWN},
};
use spotlight_platform_win::run::{KnownCommand, known_commands};
use spotlight_store::Store;

pub const ID: &str = "run";
/// How many past commands follow the one typed.
const HISTORY_MATCHES: usize = 8;
/// What `Provider::act` is asked to do to a past command.
const FORGET: &str = "forget";
/// Segoe's command prompt glyph.
const GLYPH: char = '\u{E756}';

pub struct Run {
    /// `None` without a database: nothing is remembered.
    store: Option<Arc<Mutex<Store>>>,
    prefix: RwLock<String>,
    /// Commands that run by name, by lowercase name with and without
    /// extension (`gpedit`, `gpedit.msc`).
    known: RwLock<HashMap<String, KnownCommand>>,
    /// Past commands, most recent first.
    history: RwLock<Vec<String>>,
}

impl Run {
    pub fn new(store: Option<Arc<Mutex<Store>>>, prefix: String) -> Self {
        Self {
            store,
            prefix: RwLock::new(prefix),
            known: RwLock::new(HashMap::new()),
            history: RwLock::new(Vec::new()),
        }
    }

    /// What a root search starts with to run a command; empty turns it off.
    pub fn set_prefix(&self, prefix: String) {
        if let Ok(mut current) = self.prefix.write() {
            *current = prefix;
        }
    }

    fn prefix(&self) -> String {
        self.prefix
            .read()
            .map(|prefix| prefix.clone())
            .unwrap_or_default()
    }

    /// The rows after the prefix: `rest` to run, then past commands that
    /// contain it (all of them, recent first, while nothing is typed).
    fn prefixed(&self, rest: &str) -> Vec<SearchItem> {
        let rest = rest.trim();
        let mut items = Vec::new();
        if !rest.is_empty() {
            items.push(command_item(rest, format!("Run “{rest}”"), false));
        }
        let Ok(history) = self.history.read() else {
            return items;
        };
        let lower = rest.to_lowercase();
        let (shown, limit) = if rest.is_empty() {
            (history.iter().collect::<Vec<_>>(), HISTORY_SHOWN)
        } else {
            let matching = history
                .iter()
                .filter(|line| *line != rest && line.to_lowercase().contains(&lower))
                .collect();
            (matching, HISTORY_MATCHES)
        };
        items.extend(
            shown
                .into_iter()
                .take(limit)
                .map(|line| command_item(line, line.clone(), true)),
        );
        items
    }
}

/// A row that runs `line`, titled `title`; `past` ones can be forgotten.
fn command_item(line: &str, title: String, past: bool) -> SearchItem {
    let id = item_id(ID, line);
    let mut item = SearchItem::new(id.clone(), title)
        .subtitle(if past { "Run  ·  History" } else { "Run" })
        .icon(Icon::Glyph(GLYPH));
    item = with_actions(item, line);
    if past {
        item = item
            .action(
                "Remove from History",
                Command::ProviderAction {
                    provider: ID.into(),
                    action: FORGET.into(),
                    item: id,
                },
            )
            .shortcut(shortcut::CLEAR);
    }
    item
}

fn with_actions(item: SearchItem, line: &str) -> SearchItem {
    let run = |admin| Command::Run {
        line: line.to_owned(),
        admin,
    };
    item.action("Run", run(false))
        .action("Run as Administrator", run(true))
        .action(
            "Run in Terminal",
            Command::Run {
                line: format!("cmd.exe /k {line}"),
                admin: false,
            },
        )
        .shortcut(shortcut::QUATERNARY)
        .action("Copy Command", Command::Copy(line.to_owned()))
        .shortcut(shortcut::COPY)
}

impl Provider for Run {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Run"
    }

    fn description(&self) -> &'static str {
        "Commands as in Win+R: after the prefix, or by a command's name (gpedit, regedit)."
    }

    /// Reads which commands run by name and the past commands; the one row
    /// it lists starts a command with the prefix.
    fn catalog(&self) -> Vec<SearchItem> {
        let mut known = HashMap::new();
        for command in known_commands() {
            known
                .entry(command.file.to_lowercase())
                .or_insert_with(|| command.clone());
            known.entry(command.name()).or_insert(command);
        }
        if let Ok(mut current) = self.known.write() {
            *current = known;
        }
        let history = self
            .store
            .as_ref()
            .and_then(|store| store.lock().ok()?.run_history(HISTORY_SHOWN * 5).ok())
            .unwrap_or_default();
        if let Ok(mut current) = self.history.write() {
            *current = history;
        }
        let prefix = self.prefix();
        if prefix.is_empty() {
            return Vec::new();
        }
        vec![
            SearchItem::new(item_id(ID, "start"), "Run Command")
                .subtitle(format!("Type {prefix} and a command"))
                .keywords(&["win+r", "execute", "command line", "cmd"])
                .icon(Icon::Glyph(GLYPH))
                .action("Run Command", Command::Complete(prefix)),
        ]
    }

    fn answers(&self, query: &str) -> Vec<SearchItem> {
        match run::after_prefix(&self.prefix(), query) {
            Some(rest) => self.prefixed(rest),
            None => Vec::new(),
        }
    }

    /// A command's name (`gpedit`): a row to run it, under what root search
    /// found, so an app of the same name (Notepad) stays first.
    fn fallbacks(&self, query: &str) -> Vec<SearchItem> {
        if !run::looks_like_name(query) {
            return Vec::new();
        }
        let Ok(known) = self.known.read() else {
            return Vec::new();
        };
        let Some(command) = known.get(&query.trim().to_lowercase()) else {
            return Vec::new();
        };
        let item = SearchItem::new(item_id(ID, &command.file), format!("Run {}", command.file))
            .subtitle("Run")
            .icon(Icon::Shell(command.path.clone()));
        vec![with_actions(item, &command.file)]
    }

    /// A command was run (from any of its actions): it's the most recent.
    fn opened(&self, item_id: &str) {
        let Some(line) = item_id
            .strip_prefix(ID)
            .and_then(|rest| rest.strip_prefix(':'))
        else {
            return;
        };
        if line == "start" {
            return;
        }
        if let Ok(mut history) = self.history.write() {
            history.retain(|known| known != line);
            history.insert(0, line.to_owned());
        }
        if let Some(store) = &self.store
            && let Ok(store) = store.lock()
            && let Err(err) = store.record_run(line, frecency::now())
        {
            eprintln!("[run] could not remember {line:?}: {err}");
        }
    }

    fn act(&self, action: &str, item_id: &str) -> Result<String, String> {
        let line = item_id
            .strip_prefix(ID)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(|| format!("{item_id:?} is not a command"))?;
        if action != FORGET {
            return Err(format!("Run can't {action}"));
        }
        if let Ok(mut history) = self.history.write() {
            history.retain(|known| known != line);
        }
        if let Some(store) = &self.store {
            store
                .lock()
                .map_err(|_| "The database is unavailable".to_owned())?
                .forget_run(line)
                .map_err(|err| err.to_string())?;
        }
        Ok(format!("Removed “{line}” from history"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(items: &[SearchItem]) -> Vec<&str> {
        items.iter().map(|item| item.title.as_str()).collect()
    }

    fn provider() -> Run {
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let run = Run::new(Some(store), ">".into());
        run.catalog();
        run
    }

    #[test]
    fn runs_what_follows_the_prefix_and_remembers_it() {
        let run = provider();
        assert!(titles(&run.answers(">")).is_empty());
        let typed = run.answers(">cmd /k ipconfig");
        assert_eq!(titles(&typed), ["Run “cmd /k ipconfig”"]);
        assert_eq!(
            typed[0].actions[0].command,
            Command::Run {
                line: "cmd /k ipconfig".into(),
                admin: false
            }
        );
        run.opened(&typed[0].id);
        run.opened(&item_id(ID, "gpedit.msc"));
        assert_eq!(titles(&run.answers(">")), ["gpedit.msc", "cmd /k ipconfig"]);
        // What's typed first, then the past ones that contain it.
        assert_eq!(titles(&run.answers(">cm")), ["Run “cm”", "cmd /k ipconfig"]);

        // Remembered in the database, and forgotten there too.
        let again = Run::new(run.store.clone(), ">".into());
        again.catalog();
        assert_eq!(titles(&again.answers(">")).len(), 2);
        again.act(FORGET, &item_id(ID, "gpedit.msc")).unwrap();
        assert_eq!(titles(&again.answers(">")), ["cmd /k ipconfig"]);
    }

    #[test]
    fn finds_commands_by_name() {
        let run = provider();
        let found = run.fallbacks("notepad");
        assert_eq!(titles(&found), ["Run notepad.exe"]);
        assert_eq!(titles(&run.fallbacks("NOTEPAD.exe")), ["Run notepad.exe"]);
        assert!(run.fallbacks("notepad x").is_empty());
        assert!(run.fallbacks("no-such-command-here").is_empty());
        assert!(run.answers("notepad").is_empty());
        // The prefix turned off: `>` is just text.
        run.set_prefix(String::new());
        assert!(run.answers(">cmd").is_empty());
        assert!(run.fallbacks(">cmd").is_empty());
    }
}
