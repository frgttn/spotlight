//! Games installed through launchers (Steam, Epic, GOG, Ubisoft Connect),
//! read from the launchers' own records. Games the Start menu already has a
//! shortcut for are left out: app search finds those.

use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};
use spotlight_platform_win::registry;

use crate::glyph;

pub const ID: &str = "games";

pub struct Games;

#[derive(Clone, Debug, PartialEq)]
struct Game {
    /// `steam:730`: launcher, then its id for the game.
    instance: String,
    title: String,
    launcher: &'static str,
    launch: Command,
    icon: Icon,
}

impl Provider for Games {
    fn id(&self) -> &'static str {
        ID
    }

    /// Reads a few small files and registry keys; a few milliseconds.
    fn catalog(&self) -> Vec<SearchItem> {
        let shortcuts = start_menu_names();
        let mut seen = HashSet::new();
        [steam(), epic(), gog(), ubisoft()]
            .into_iter()
            .flatten()
            .filter(|game| {
                let name = normalize(&game.title);
                !shortcuts.contains(&name) && seen.insert(name)
            })
            .map(|game| {
                SearchItem::new(item_id(ID, &game.instance), game.title)
                    .subtitle(format!("{} game", game.launcher))
                    .keywords(&["game", game.launcher])
                    .icon(game.icon)
                    .action("Play", game.launch)
            })
            .collect()
    }
}

/// Letters and digits only, lower case: "PUBG: BATTLEGROUNDS" and the
/// shortcut "PUBG BATTLEGROUNDS.url" are the same game.
fn normalize(title: &str) -> String {
    title
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Names of the Start menu's shortcuts (per user and for everyone), normalized.
fn start_menu_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for (variable, relative) in [
        ("APPDATA", r"Microsoft\Windows\Start Menu\Programs"),
        ("PROGRAMDATA", r"Microsoft\Windows\Start Menu\Programs"),
    ] {
        if let Some(base) = env::var_os(variable) {
            collect_shortcuts(&PathBuf::from(base).join(relative), &mut names, 0);
        }
    }
    names
}

fn collect_shortcuts(dir: &Path, names: &mut HashSet<String>, depth: usize) {
    // The Start menu is a shallow tree; this bounds a cycle through a link.
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_shortcuts(&path, names, depth + 1);
        } else if path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("lnk") || extension.eq_ignore_ascii_case("url")
        }) && let Some(stem) = path.file_stem()
        {
            names.insert(normalize(&stem.to_string_lossy()));
        }
    }
}

// --- Steam --------------------------------------------------------------------

/// Steam's own tools that it installs like games.
const STEAM_TOOLS: &[&str] = &["228980"];

fn steam() -> Vec<Game> {
    let Some(root) = registry::string(r"HKCU\Software\Valve\Steam", "SteamPath") else {
        return Vec::new();
    };
    // Steam writes its path with forward slashes, which the shell's icon lookup rejects.
    let root = PathBuf::from(root.replace('/', "\\"));
    let icon = Icon::Shell(root.join("steam.exe").to_string_lossy().into_owned());
    let mut libraries = vec![root.clone()];
    if let Ok(text) = fs::read_to_string(root.join(r"steamapps\libraryfolders.vdf")) {
        libraries.extend(vdf_values(&text, "path").into_iter().map(PathBuf::from));
    }
    libraries.sort();
    libraries.dedup();

    let mut games = Vec::new();
    for library in libraries {
        let Ok(entries) = fs::read_dir(library.join("steamapps")) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(entry.path())
                && let Some(game) = steam_game(&text, &icon)
            {
                games.push(game);
            }
        }
    }
    games
}

/// A game from an `appmanifest_<id>.acf`, if it's installed and not a tool.
fn steam_game(manifest: &str, icon: &Icon) -> Option<Game> {
    let id = vdf_values(manifest, "appid").into_iter().next()?;
    let title = vdf_values(manifest, "name").into_iter().next()?;
    // StateFlags bit 2: fully installed (not queued or half downloaded).
    let flags: u32 = vdf_values(manifest, "StateFlags").first()?.parse().ok()?;
    let tool = STEAM_TOOLS.contains(&id.as_str())
        || title.contains("Redistributable")
        || title.starts_with("Proton ")
        || title.starts_with("Steam Linux Runtime");
    (flags & 4 != 0 && !tool).then(|| Game {
        instance: format!("steam:{id}"),
        launch: Command::OpenUrl(format!("steam://rungameid/{id}")),
        title,
        launcher: "Steam",
        icon: icon.clone(),
    })
}

/// Every value of `key` in Valve's KeyValues text (`"key"  "value"` lines),
/// at any depth, with `\\` unescaped.
fn vdf_values(text: &str, key: &str) -> Vec<String> {
    let quoted = format!("\"{key}\"");
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix(&quoted)?.trim();
            let value = rest.strip_prefix('"')?.strip_suffix('"')?;
            Some(value.replace(r"\\", r"\"))
        })
        .collect()
}

// --- Epic ---------------------------------------------------------------------

fn epic() -> Vec<Game> {
    let Some(data) = env::var_os("PROGRAMDATA") else {
        return Vec::new();
    };
    let dir = PathBuf::from(data).join(r"Epic\EpicGamesLauncher\Data\Manifests");
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "item")
        })
        .filter_map(|entry| epic_game(&fs::read_to_string(entry.path()).ok()?))
        .collect()
}

/// A game from an Epic launcher `.item` manifest (JSON).
fn epic_game(manifest: &str) -> Option<Game> {
    let json: serde_json::Value = serde_json::from_str(manifest).ok()?;
    let text = |key: &str| json[key].as_str().map(str::to_owned);
    if json["bIsIncompleteInstall"].as_bool() == Some(true) {
        return None;
    }
    // Engines and plugins install the same way; only keep games.
    if let Some(categories) = json["AppCategories"].as_array()
        && !categories.iter().any(|category| category == "games")
    {
        return None;
    }
    let (title, app) = (text("DisplayName")?, text("AppName")?);
    let (namespace, item) = (text("CatalogNamespace")?, text("CatalogItemId")?);
    let program = Path::new(&text("InstallLocation")?).join(text("LaunchExecutable")?);
    Some(Game {
        instance: format!("epic:{app}"),
        title,
        launcher: "Epic",
        launch: Command::OpenUrl(format!(
            "com.epicgames.launcher://apps/{namespace}%3A{item}%3A{app}?action=launch&silent=true"
        )),
        icon: Icon::Shell(program.to_string_lossy().into_owned()),
    })
}

// --- GOG and Ubisoft: both keep a registry key per installed game -------------

fn gog() -> Vec<Game> {
    const GAMES: &str = r"HKLM\SOFTWARE\WOW6432Node\GOG.com\Games";
    registry::subkeys(GAMES)
        .into_iter()
        .filter_map(|id| {
            let key = format!(r"{GAMES}\{id}");
            let title = registry::string(&key, "gameName")?;
            let program = registry::string(&key, "exe")?;
            Path::new(&program).is_file().then(|| Game {
                instance: format!("gog:{id}"),
                title,
                launcher: "GOG",
                launch: Command::Open(program.clone()),
                icon: Icon::Shell(program),
            })
        })
        .collect()
}

fn ubisoft() -> Vec<Game> {
    const INSTALLS: &str = r"HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs";
    registry::subkeys(INSTALLS)
        .into_iter()
        .filter_map(|id| {
            let folder = registry::string(&format!(r"{INSTALLS}\{id}"), "InstallDir")?;
            // Ubisoft keeps no display name; the install folder is named after the game.
            let title = Path::new(folder.trim_end_matches(['/', '\\']))
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(Game {
                instance: format!("ubisoft:{id}"),
                title,
                launcher: "Ubisoft",
                launch: Command::OpenUrl(format!("uplay://launch/{id}/0")),
                icon: Icon::Glyph(glyph::GAME),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_steam_manifests() {
        let manifest = r#""AppState"
{
	"appid"		"570"
	"name"		"Dota 2"
	"StateFlags"		"4"
	"installdir"		"dota 2 beta"
}"#;
        let icon = Icon::None;
        let game = steam_game(manifest, &icon).unwrap();
        assert_eq!(game.title, "Dota 2");
        assert_eq!(
            game.launch,
            Command::OpenUrl("steam://rungameid/570".into())
        );

        let downloading = manifest.replace(r#""StateFlags"		"4""#, r#""StateFlags"		"1026""#);
        assert_eq!(steam_game(&downloading, &icon), None);
        let redist = manifest
            .replace("570", "228980")
            .replace("Dota 2", "Steamworks Common Redistributables");
        assert_eq!(steam_game(&redist, &icon), None);
    }

    #[test]
    fn reads_steam_library_paths() {
        let folders = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps" { "228980" "1" }
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(
            vdf_values(folders, "path"),
            [r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]
        );
    }

    #[test]
    fn reads_epic_manifests() {
        let manifest = r#"{
            "DisplayName": "Football Manager 2024",
            "AppName": "da20",
            "CatalogNamespace": "610a",
            "CatalogItemId": "594b",
            "InstallLocation": "C:\\Program Files\\Epic Games\\FootballManager24",
            "LaunchExecutable": "fm.exe",
            "bIsIncompleteInstall": false,
            "AppCategories": ["public", "games", "applications"]
        }"#;
        let game = epic_game(manifest).unwrap();
        assert_eq!(game.title, "Football Manager 2024");
        assert_eq!(
            game.launch,
            Command::OpenUrl(
                "com.epicgames.launcher://apps/610a%3A594b%3Ada20?action=launch&silent=true".into()
            )
        );
        assert_eq!(
            game.icon,
            Icon::Shell(r"C:\Program Files\Epic Games\FootballManager24\fm.exe".into())
        );
        let engine = manifest.replace(r#""games", "#, "");
        assert_eq!(epic_game(&engine), None);
        let half = manifest.replace(
            r#""bIsIncompleteInstall": false"#,
            r#""bIsIncompleteInstall": true"#,
        );
        assert_eq!(epic_game(&half), None);
    }

    #[test]
    fn matches_shortcut_names() {
        assert_eq!(
            normalize("PUBG: BATTLEGROUNDS"),
            normalize("PUBG BATTLEGROUNDS")
        );
        assert_eq!(normalize("Battlefield™ 6"), "battlefield6");
    }

    /// Read-only: what this machine's launchers have.
    #[test]
    fn lists_this_machines_games() {
        let items = Games.catalog();
        for item in &items {
            eprintln!(
                "{:<32} {:<14} {:?}",
                item.title,
                item.subtitle.as_deref().unwrap_or(""),
                item.actions[0].command
            );
        }
    }
}
