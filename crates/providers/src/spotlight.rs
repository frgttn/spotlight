//! Commands about Spotlight itself.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
    view::{DetailView, View},
};

use crate::glyph;

pub const ID: &str = "spotlight";

pub struct Spotlight;

const SHORTCUTS: &str = "\
## Search

- **↑ / ↓** or **Ctrl+N / Ctrl+P** move the selection
- **Enter** runs the selected item; **Ctrl+Enter** runs its second action
- **Ctrl+K** shows every action of the selected item
- **Esc** goes back one view, then closes the launcher

## Search field

- **Ctrl+← / Ctrl+→** move by word, with **Shift** to select
- **Ctrl+Backspace / Ctrl+Delete** delete a word
- **Ctrl+A**, **Ctrl+C**, **Ctrl+X**, **Ctrl+V** as usual

## Grids and forms

- **Arrow keys** move between tiles
- **Tab / Shift+Tab** move between fields; **Enter** submits

## Anywhere

- **Ctrl+,** opens Settings, **Ctrl+Q** quits
- Give any app or command its own **alias** or global **hotkey** with *Set Alias…* and *Set Hotkey…* in **Ctrl+K**

## Quicklinks

- **!gh tokio** (or **tokio !gh**) opens the quicklink with keyword *gh* with “tokio” filled in, or else one of ~10,000 search bangs (Kagi's, as in Helium)
- **!g** lists the keywords starting with *g*; **Enter** completes one; *Search Bangs* lists them all
- *Create Quicklink* saves a link of your own: put **{query}** where the text goes, or **{argument name=\"Repo\"}** for several values
- **{date}**, **{time}** and **{uuid}** are filled in when the link opens
";

impl Provider for Spotlight {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Spotlight Commands"
    }

    fn description(&self) -> &'static str {
        "Commands about Spotlight itself, such as Keyboard Shortcuts."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let shortcuts = View::Detail(DetailView {
            title: "Keyboard Shortcuts".into(),
            markdown: SHORTCUTS.into(),
            metadata: Vec::new(),
            actions: Vec::new(),
        });
        vec![
            SearchItem::new(item_id(ID, "shortcuts"), "Keyboard Shortcuts")
                .subtitle("Spotlight")
                .keywords(&["help", "keys", "hotkeys"])
                .icon(Icon::Glyph(glyph::KEYBOARD))
                .action("Show Shortcuts", Command::PushView(Box::new(shortcuts))),
        ]
    }
}
