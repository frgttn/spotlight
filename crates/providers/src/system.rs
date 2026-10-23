//! System commands: lock, sleep, restart, Recycle Bin, theme, sound.

use spotlight_core::{
    item::{Command, Icon, SearchItem, SystemCommand},
    item_id,
    provider::Provider,
};

use crate::glyph;

pub const ID: &str = "system";

pub struct System;

/// (instance id, title, keywords, glyph, command)
const COMMANDS: &[(&str, &str, &[&str], char, SystemCommand)] = &[
    (
        "lock",
        "Lock Screen",
        &["lock computer"],
        glyph::LOCK,
        SystemCommand::Lock,
    ),
    (
        "sleep",
        "Sleep",
        &["suspend", "standby"],
        glyph::SLEEP,
        SystemCommand::Sleep,
    ),
    (
        "restart",
        "Restart",
        &["reboot"],
        glyph::RESTART,
        SystemCommand::Restart,
    ),
    (
        "shut-down",
        "Shut Down",
        &["shutdown", "power off", "turn off"],
        glyph::POWER,
        SystemCommand::ShutDown,
    ),
    (
        "sign-out",
        "Sign Out",
        &["log off", "log out", "logout"],
        glyph::SIGN_OUT,
        SystemCommand::SignOut,
    ),
    (
        "empty-recycle-bin",
        "Empty Recycle Bin",
        &["trash"],
        glyph::DELETE,
        SystemCommand::EmptyRecycleBin,
    ),
    (
        "toggle-dark-mode",
        "Toggle Dark Mode",
        &["light mode", "theme", "appearance"],
        glyph::BRIGHTNESS,
        SystemCommand::ToggleDarkMode,
    ),
    (
        "toggle-mute",
        "Toggle Mute",
        &["unmute", "sound", "volume", "audio"],
        glyph::MUTE,
        SystemCommand::ToggleMute,
    ),
];

impl Provider for System {
    fn id(&self) -> &'static str {
        ID
    }

    fn catalog(&self) -> Vec<SearchItem> {
        COMMANDS
            .iter()
            .map(|&(instance, title, keywords, glyph, command)| {
                SearchItem::new(item_id(ID, instance), title)
                    .subtitle("System")
                    .keywords(keywords)
                    .icon(Icon::Glyph(glyph))
                    .action(title, Command::System(command))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_command_is_listed() {
        let items = System.catalog();
        let mut ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), items.len());
        assert!(items.iter().all(|item| item.primary_action().is_some()));
    }

    #[test]
    fn power_commands_ask_for_confirmation() {
        let confirm: Vec<_> = System
            .catalog()
            .into_iter()
            .filter(|item| item.actions[0].command.needs_confirmation())
            .map(|item| item.title)
            .collect();
        assert_eq!(confirm, ["Restart", "Shut Down", "Sign Out"]);
    }
}
