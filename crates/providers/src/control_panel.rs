//! Control Panel items (Device Manager, Mouse, Credential Manager…), as the
//! shell lists them on this machine, with their own icons.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::apps;

pub const ID: &str = "control-panel";

pub struct ControlPanel;

impl Provider for ControlPanel {
    fn id(&self) -> &'static str {
        ID
    }

    /// Blocking and COM-heavy (tens of ms), like app discovery.
    fn catalog(&self) -> Vec<SearchItem> {
        let items = match apps::control_panel() {
            Ok(items) => items,
            Err(err) => {
                eprintln!("[control panel] could not list items: {err}");
                return Vec::new();
            }
        };
        items
            .into_iter()
            // Items without a display name show their parsing name instead.
            .filter(|item| !item.name.starts_with("::"))
            .map(|item| {
                // The last `::{GUID}` names the item and stays put across runs.
                let instance = item
                    .parsing_name
                    .rsplit('\\')
                    .next()
                    .unwrap_or(&item.parsing_name)
                    .to_owned();
                SearchItem::new(item_id(ID, &instance), item.name)
                    .subtitle("Control Panel")
                    .icon(Icon::Shell(item.parsing_name.clone()))
                    .action("Open", Command::Open(item.parsing_name))
            })
            .collect()
    }
}
