//! System commands: power and session, Recycle Bin, theme, sound, Bluetooth,
//! HDR, night light, drives.

use spotlight_core::{
    item::{Command, Icon, SearchItem, SystemCommand},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::drives::{self, DriveKind};

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
        &["unmute", "sound", "audio"],
        glyph::MUTE,
        SystemCommand::ToggleMute,
    ),
    (
        "volume-up",
        "Volume Up",
        &["louder", "sound"],
        glyph::VOLUME_UP,
        SystemCommand::VolumeUp,
    ),
    (
        "volume-down",
        "Volume Down",
        &["quieter", "sound"],
        glyph::VOLUME_DOWN,
        SystemCommand::VolumeDown,
    ),
    (
        "toggle-bluetooth",
        "Toggle Bluetooth",
        &["bluetooth on", "bluetooth off", "wireless"],
        glyph::BLUETOOTH,
        SystemCommand::ToggleBluetooth,
    ),
    (
        "toggle-hdr",
        "Toggle HDR",
        &["high dynamic range", "display"],
        glyph::DISPLAY,
        SystemCommand::ToggleHdr,
    ),
    (
        "eject-all",
        "Eject All Drives",
        &["usb", "safely remove", "unmount"],
        glyph::USB,
        SystemCommand::EjectAll,
    ),
];

/// Windows has no public way to switch night light, only its Settings page.
const NIGHT_LIGHT_SETTINGS: &str = "ms-settings:nightlight";

impl Provider for System {
    fn id(&self) -> &'static str {
        ID
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let mut items: Vec<SearchItem> = COMMANDS
            .iter()
            .map(|&(instance, title, keywords, glyph, command)| {
                SearchItem::new(item_id(ID, instance), title)
                    .subtitle("System")
                    .keywords(keywords)
                    .icon(Icon::Glyph(glyph))
                    .action(title, Command::System(command))
            })
            .collect();
        items.push(
            SearchItem::new(item_id(ID, "night-light"), "Night Light Settings")
                .subtitle("System")
                .keywords(&["blue light", "warm", "night mode"])
                .icon(Icon::Glyph(glyph::NIGHT_LIGHT))
                .action(
                    "Open Night Light Settings",
                    Command::OpenUrl(NIGHT_LIGHT_SETTINGS.into()),
                ),
        );
        items
    }

    /// "volume 40" sets the volume; "eject" lists each removable drive.
    fn answers(&self, query: &str) -> Vec<SearchItem> {
        if let Some(percent) = volume_level(query) {
            return vec![
                SearchItem::new(
                    item_id(ID, "set-volume"),
                    format!("Set Volume to {percent}%"),
                )
                .subtitle("System")
                .icon(Icon::Glyph(glyph::VOLUME))
                .action(
                    "Set Volume",
                    Command::System(SystemCommand::SetVolume(percent)),
                ),
            ];
        }
        if wants_eject(query) {
            return drives::removable()
                .into_iter()
                .map(|drive| {
                    let letter = drive.letter;
                    let what = match drive.kind {
                        DriveKind::Removable => "Removable drive",
                        DriveKind::Optical => "Optical drive",
                    };
                    SearchItem::new(
                        item_id(ID, &format!("eject-{letter}")),
                        format!("Eject {} ({letter}:)", drive.label),
                    )
                    .subtitle(what)
                    .icon(Icon::Glyph(glyph::USB))
                    .action("Eject", Command::System(SystemCommand::Eject(letter)))
                })
                .collect();
        }
        Vec::new()
    }
}

/// `"volume 40"`, `"vol 40%"` → 40. Levels above 100 aren't one.
fn volume_level(query: &str) -> Option<u8> {
    let lower = query.trim().to_lowercase();
    let rest = lower
        .strip_prefix("volume")
        .or_else(|| lower.strip_prefix("vol"))?;
    // "volume40" is fine, "volumes" isn't a level.
    let number = rest.trim().trim_end_matches('%').trim_end();
    let level: u8 = number.parse().ok()?;
    (level <= 100).then_some(level)
}

/// The query is the start of "eject" (at least "ej"), or "eject" and more.
fn wants_eject(query: &str) -> bool {
    let lower = query.trim().to_lowercase();
    (lower.len() >= 2 && "eject".starts_with(&lower)) || lower.starts_with("eject ")
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

    #[test]
    fn reads_volume_levels() {
        assert_eq!(volume_level("volume 40"), Some(40));
        assert_eq!(volume_level("Vol 5%"), Some(5));
        assert_eq!(volume_level("volume100"), Some(100));
        assert_eq!(volume_level("volume 0"), Some(0));
        for query in ["volume", "volume 101", "volumes 3", "volume -5", "sound 40"] {
            assert_eq!(volume_level(query), None, "{query:?}");
        }
    }

    #[test]
    fn set_volume_answer() {
        let answers = System.answers("volume 25");
        assert_eq!(
            answers[0].actions[0].command,
            Command::System(SystemCommand::SetVolume(25))
        );
    }

    #[test]
    fn recognises_eject_queries() {
        for query in ["ej", "eje", "Eject", "eject e"] {
            assert!(wants_eject(query), "{query:?}");
        }
        for query in ["e", "eclipse", "ejector seat", "reject"] {
            assert!(!wants_eject(query), "{query:?}");
        }
    }
}
