//! Installed apps, Win32 and MSIX alike, from the shell's `AppsFolder`.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::apps;

pub const ID: &str = "apps";

pub struct Apps;

impl Provider for Apps {
    fn id(&self) -> &'static str {
        ID
    }

    /// Blocking and COM-heavy (~200 ms): the shell enumerates every app.
    fn catalog(&self) -> Vec<SearchItem> {
        let apps = match apps::discover() {
            Ok(apps) => apps,
            Err(err) => {
                eprintln!("[apps] discovery failed: {err}");
                return Vec::new();
            }
        };
        apps.into_iter()
            .map(|app| {
                // Same id as before providers existed, so recorded frecency still applies.
                SearchItem::new(item_id(ID, &app.launch_id), app.name)
                    .subtitle("Application")
                    .icon(Icon::App(app.launch_id.clone()))
                    .action("Open", Command::LaunchApp(app.launch_id))
            })
            .collect()
    }
}
