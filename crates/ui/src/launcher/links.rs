//! `spotlight://` links in the launcher. A link from outside (a browser, a
//! shortcut) opens the launcher on the command it names and waits for Enter,
//! so a web page can't run commands by itself; Ctrl+Enter also lets that
//! command open from links without asking from then on, as Raycast's
//! "Always Open Command" does. A link opened from inside (the user's own
//! quicklink) runs at once, as Raycast's internal links do; commands that
//! ask for a second Enter still wait for it.

use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Window};
use spotlight_core::{
    deeplink::{self, CommandLink, Deeplink},
    item::Command,
    view::View,
};
use spotlight_search::Row;

use super::{Launcher, views::Screen};

/// `settings` key: ids of the items that open from links without asking,
/// one per line.
pub(super) const TRUSTED_SETTING: &str = "deeplink_trusted";
/// How long a link that arrives while the catalogs load (it started
/// Spotlight) waits for them.
const LOAD_WAIT: Duration = Duration::from_secs(10);

/// A command a link named, waiting for Enter.
pub(super) struct PendingLink {
    pub item_id: String,
    pub link: CommandLink,
}

/// Reads [`TRUSTED_SETTING`].
pub(super) fn parse_trusted(setting: &str) -> impl Iterator<Item = String> + '_ {
    setting
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

impl Launcher {
    /// Opens a `spotlight://` link: the launcher, or the command it names.
    pub fn open_link(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_link_after_load(text.to_owned(), true, false, window, cx);
    }

    /// Opens a `spotlight://` link a quicklink of the user's own named.
    pub(super) fn open_own_link(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_link_after_load(text.to_owned(), false, true, window, cx);
    }

    /// `own`: the link came from inside the launcher, so it's trusted.
    fn open_link_after_load(
        &mut self,
        text: String,
        wait: bool,
        own: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = match deeplink::parse(&text) {
            Ok(Deeplink::Open) => return self.open(window, cx),
            Ok(Deeplink::Command(link)) => link,
            Err(err) => {
                eprintln!("[spotlight] can't open {text:?}: {err}");
                return self.link_problem(format!("Can't open this link: {err}"), cx);
            }
        };
        let found = self
            .search
            .catalog()
            .iter()
            .position(|item| link.names(&item.id, &item.title));
        let Some(ix) = found else {
            // The link started Spotlight: its command may not be loaded yet.
            if wait && self.loading > 0 {
                return self.wait_for_catalogs(text, window, cx);
            }
            let off = self
                .sources()
                .into_iter()
                .find(|source| source.id == link.provider && !source.enabled);
            let message = match off {
                Some(source) => format!("{} is turned off in Settings", source.name),
                None => "No command matches this link".into(),
            };
            return self.link_problem(message, cx);
        };
        let id = self.search.catalog()[ix].id.clone();
        let asks = self
            .link_command(ix, &link)
            .is_some_and(|command| command.needs_confirmation());
        if (own || self.trusted_links.contains(&id)) && !asks {
            return self.run_link(ix, &link, window, cx);
        }
        self.show(Instant::now(), window, cx);
        self.pending_link = Some(PendingLink { item_id: id, link });
        self.pin_pending_link();
        cx.notify();
    }

    /// Tries `text` again once every catalog has loaded (or after
    /// [`LOAD_WAIT`]).
    fn wait_for_catalogs(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            while started.elapsed() < LOAD_WAIT {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                match this.read_with(cx, |this, _| this.loading) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            cx.update_window(handle, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.open_link_after_load(text, false, false, window, cx)
                })
            })
            .ok();
        })
        .detach();
    }

    /// What item `ix`'s primary action becomes with `link`'s launch context
    /// and arguments. A command that takes arguments without asking for
    /// them (a quicklink with defaults) takes them through its provider's
    /// form ([`deeplink::ARGUMENTS_VIEW`]).
    fn link_command(&self, ix: usize, link: &CommandLink) -> Option<Command> {
        let item = &self.search.catalog()[ix];
        let mut command = item.primary_action()?.command.clone();
        let is_form =
            matches!(&command, Command::PushView(view) if matches!(**view, View::Form(_)));
        if !link.arguments.is_empty()
            && !is_form
            && let Some((provider, instance)) = item.id.split_once(':')
            && let Some(form) = self
                .search
                .providers()
                .iter()
                .find(|candidate| candidate.id() == provider)
                .and_then(|provider| {
                    provider.view(&format!("{}{instance}", deeplink::ARGUMENTS_VIEW))
                })
        {
            command = Command::PushView(Box::new(form));
        }
        let command = deeplink::apply_context(command, link.context.as_deref());
        Some(deeplink::apply_arguments(command, &link.arguments))
    }

    /// Runs item `ix`'s primary action as `link` asks: with its arguments,
    /// and its fallback text in the search field of a list it opens.
    fn run_link(
        &mut self,
        ix: usize,
        link: &CommandLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.search.catalog()[ix].id.clone();
        let Some(command) = self.link_command(ix, link) else {
            return;
        };
        let opens_view = super::opens_view(&command);
        if !self.visible {
            // Window commands act on the window the link was opened from.
            self.previous_window = spotlight_platform_win::clipboard::foreground_window();
            if opens_view {
                self.show(Instant::now(), window, cx);
            }
        }
        self.pending_link = None;
        self.record_visit(id, cx);
        self.perform(&command, window, cx);
        if let Some(text) = &link.fallback_text
            && opens_view
            && matches!(self.screens.last(), Some(Screen::List { .. }))
        {
            let text = text.clone();
            self.input.update(cx, |input, cx| input.type_over(text, cx));
        }
    }

    /// Enter (or Ctrl+Enter, `always`) on the command a link named.
    pub(super) fn open_pending_link(
        &mut self,
        always: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_link.take() else {
            return;
        };
        let Some(ix) = self
            .search
            .catalog()
            .iter()
            .position(|item| item.id == pending.item_id)
        else {
            return self.show_toast("That command is gone".into(), cx);
        };
        if always {
            self.trusted_links.insert(pending.item_id);
            self.save_trusted_links(cx);
        }
        self.run_link(ix, &pending.link, window, cx);
    }

    /// The command `id` asks again before it opens from a link (its action
    /// panel, or Settings).
    pub fn distrust_link(&mut self, id: &str, cx: &mut Context<Self>) {
        self.trusted_links.remove(id);
        self.save_trusted_links(cx);
    }

    /// Shows only the command a link named, after the rows were rebuilt (a
    /// catalog finished loading under it).
    pub(super) fn pin_pending_link(&mut self) {
        let Some(pending) = &self.pending_link else {
            return;
        };
        if let Some(ix) = self
            .search
            .catalog()
            .iter()
            .position(|item| item.id == pending.item_id)
        {
            self.results = vec![Row::Catalog(ix)];
            self.sections.clear();
            self.selected = 0;
            self.panel = None;
        }
    }

    /// Whether `id` is the command a link is waiting on.
    pub(super) fn link_pending_for(&self, id: &str) -> bool {
        self.pending_link
            .as_ref()
            .is_some_and(|pending| pending.item_id == id)
    }

    fn save_trusted_links(&self, cx: &mut Context<Self>) {
        let mut ids: Vec<&String> = self.trusted_links.iter().collect();
        ids.sort_unstable();
        let value = ids
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        self.persist(cx, move |store| store.set_setting(TRUSTED_SETTING, &value));
    }

    fn link_problem(&mut self, message: String, cx: &mut Context<Self>) {
        if self.visible {
            self.show_toast(message.into(), cx);
        } else {
            self.show_hud(message.into(), cx);
        }
    }
}
