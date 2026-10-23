//! Pages of the Windows Settings app, opened directly by their `ms-settings:`
//! URIs. A page a Windows version doesn't have opens Settings' home page.

use spotlight_core::{
    item::{Command, Icon, SearchItem},
    item_id,
    provider::Provider,
};

pub const ID: &str = "settings";

/// The Settings app, for every page's icon.
const SETTINGS_APP: &str =
    "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

/// (URI after `ms-settings:`, title as Windows 11 names the page, keywords)
const PAGES: &[(&str, &str, &[&str])] = &[
    // System
    (
        "display",
        "Display",
        &["screen", "resolution", "scale", "brightness"],
    ),
    (
        "display-advancedgraphics",
        "Graphics",
        &["gpu", "graphics preference"],
    ),
    ("sound", "Sound", &["audio", "speakers", "output", "input"]),
    (
        "sound-devices",
        "Sound Devices",
        &["audio devices", "microphone"],
    ),
    ("apps-volume", "Volume Mixer", &["app volume", "sound"]),
    ("notifications", "Notifications", &["alerts", "banners"]),
    (
        "quiethours",
        "Focus and Do Not Disturb",
        &["focus assist", "dnd"],
    ),
    (
        "powersleep",
        "Power & Battery",
        &["sleep", "screen timeout", "power mode"],
    ),
    ("batterysaver", "Energy Saver", &["battery saver"]),
    (
        "storagesense",
        "Storage",
        &["disk space", "storage sense", "cleanup"],
    ),
    (
        "multitasking",
        "Multitasking",
        &["snap windows", "alt tab", "virtual desktops"],
    ),
    ("project", "Projecting to This PC", &["miracast"]),
    ("clipboard", "Clipboard Settings", &["clipboard history"]),
    ("remotedesktop", "Remote Desktop", &["rdp"]),
    ("troubleshoot", "Troubleshoot", &["fix problems"]),
    (
        "recovery",
        "Recovery",
        &["reset this pc", "advanced startup"],
    ),
    ("activation", "Activation", &["product key", "license"]),
    (
        "about",
        "About This PC",
        &["system info", "pc name", "specs", "rename pc"],
    ),
    ("developers", "For Developers", &["developer mode", "sudo"]),
    // Bluetooth & devices
    (
        "bluetooth",
        "Bluetooth & Devices",
        &["pair", "connect device"],
    ),
    ("printers", "Printers & Scanners", &["print"]),
    ("mousetouchpad", "Mouse", &["pointer speed", "scroll"]),
    ("devices-touchpad", "Touchpad", &["gestures"]),
    ("typing", "Typing", &["autocorrect", "touch keyboard"]),
    ("pen", "Pen & Windows Ink", &["stylus"]),
    ("autoplay", "AutoPlay", &["removable drives"]),
    ("usb", "USB", &["usb notifications"]),
    ("camera", "Cameras", &["webcam"]),
    // Network & internet
    (
        "network-status",
        "Network & Internet",
        &["connection", "network status"],
    ),
    ("network-wifi", "Wi-Fi", &["wifi", "wireless"]),
    ("network-ethernet", "Ethernet", &["wired", "lan"]),
    ("network-vpn", "VPN", &["vpn connection"]),
    (
        "network-mobilehotspot",
        "Mobile Hotspot",
        &["hotspot", "share internet"],
    ),
    ("network-airplanemode", "Airplane Mode", &["flight mode"]),
    ("network-proxy", "Proxy", &["proxy server"]),
    ("datausage", "Data Usage", &["metered", "data limit"]),
    (
        "network-advancedsettings",
        "Advanced Network Settings",
        &["adapters", "network reset"],
    ),
    // Personalization
    (
        "personalization-background",
        "Background",
        &["wallpaper", "desktop picture"],
    ),
    (
        "colors",
        "Colors",
        &["accent color", "dark mode", "transparency"],
    ),
    ("themes", "Themes", &["desktop icons"]),
    (
        "lockscreen",
        "Lock Screen Settings",
        &["lock screen picture"],
    ),
    (
        "personalization-start",
        "Start Menu Settings",
        &["start layout"],
    ),
    (
        "taskbar",
        "Taskbar",
        &["taskbar alignment", "system tray", "taskbar icons"],
    ),
    ("fonts", "Fonts", &["typefaces", "install font"]),
    ("deviceusage", "Device Usage", &["personalized tips"]),
    // Apps
    (
        "appsfeatures",
        "Installed Apps",
        &["uninstall", "programs", "apps and features"],
    ),
    (
        "defaultapps",
        "Default Apps",
        &["default browser", "open with", "file associations"],
    ),
    (
        "optionalfeatures",
        "Optional Features",
        &["add feature", "windows features"],
    ),
    ("appsforwebsites", "Apps for Websites", &["app links"]),
    ("videoplayback", "Video Playback", &["hdr video"]),
    (
        "startupapps",
        "Startup Apps",
        &["autostart", "run at login"],
    ),
    // Accounts
    (
        "yourinfo",
        "Your Info",
        &["account picture", "microsoft account"],
    ),
    ("emailandaccounts", "Email & Accounts", &["mail accounts"]),
    (
        "signinoptions",
        "Sign-in Options",
        &["password", "pin", "windows hello", "fingerprint"],
    ),
    (
        "workplace",
        "Access Work or School",
        &["work account", "mdm"],
    ),
    ("otherusers", "Other Users", &["family", "add user"]),
    ("backup", "Windows Backup", &["onedrive backup"]),
    // Time & language
    ("dateandtime", "Date & Time", &["clock", "time zone"]),
    (
        "regionlanguage",
        "Language & Region",
        &["display language", "region", "format"],
    ),
    ("keyboard", "Keyboard Settings", &["input method", "layout"]),
    ("speech", "Speech", &["voice", "text to speech"]),
    // Gaming
    ("gaming-gamebar", "Game Bar", &["xbox game bar"]),
    (
        "gaming-gamedvr",
        "Captures",
        &["game recording", "screenshots"],
    ),
    ("gaming-gamemode", "Game Mode", &["gaming performance"]),
    // Accessibility
    (
        "easeofaccess-display",
        "Text Size",
        &["accessibility", "bigger text"],
    ),
    (
        "easeofaccess-visualeffects",
        "Visual Effects",
        &["animations", "scrollbars", "transparency"],
    ),
    (
        "easeofaccess-mousepointer",
        "Mouse Pointer and Touch",
        &["cursor size", "pointer color"],
    ),
    (
        "easeofaccess-cursor",
        "Text Cursor",
        &["caret", "cursor thickness"],
    ),
    (
        "easeofaccess-magnifier",
        "Magnifier",
        &["zoom", "accessibility"],
    ),
    (
        "easeofaccess-colorfilter",
        "Color Filters",
        &["color blind", "grayscale"],
    ),
    (
        "easeofaccess-highcontrast",
        "Contrast Themes",
        &["high contrast"],
    ),
    ("easeofaccess-narrator", "Narrator", &["screen reader"]),
    ("easeofaccess-audio", "Accessibility Audio", &["mono audio"]),
    (
        "easeofaccess-closedcaptioning",
        "Captions",
        &["subtitles", "live captions"],
    ),
    (
        "easeofaccess-speechrecognition",
        "Voice Access",
        &["speech recognition", "dictation"],
    ),
    (
        "easeofaccess-keyboard",
        "Accessibility Keyboard",
        &["sticky keys", "filter keys", "on-screen keyboard"],
    ),
    (
        "easeofaccess-mouse",
        "Mouse Keys",
        &["numeric keypad mouse"],
    ),
    ("easeofaccess-eyecontrol", "Eye Control", &["eye tracking"]),
    // Privacy & security
    ("privacy", "Privacy & Security", &["permissions"]),
    (
        "windowsdefender",
        "Windows Security",
        &["antivirus", "defender", "firewall"],
    ),
    ("findmydevice", "Find My Device", &["lost device"]),
    (
        "privacy-location",
        "Location",
        &["gps", "location services"],
    ),
    ("privacy-webcam", "Camera Privacy", &["camera access"]),
    ("privacy-microphone", "Microphone Privacy", &["mic access"]),
    (
        "privacy-speech",
        "Speech Privacy",
        &["online speech recognition"],
    ),
    (
        "privacy-feedback",
        "Diagnostics & Feedback",
        &["telemetry", "diagnostic data"],
    ),
    ("privacy-activityhistory", "Activity History", &["timeline"]),
    ("privacy-general", "General Privacy", &["advertising id"]),
    // Windows Update
    (
        "windowsupdate",
        "Windows Update",
        &["updates", "check for updates"],
    ),
    (
        "windowsupdate-history",
        "Update History",
        &["installed updates"],
    ),
    (
        "windowsupdate-options",
        "Advanced Update Options",
        &["active hours", "optional updates"],
    ),
    (
        "windowsinsider",
        "Windows Insider Program",
        &["insider", "preview builds"],
    ),
    (
        "delivery-optimization",
        "Delivery Optimization",
        &["download bandwidth"],
    ),
];

pub struct SettingsPages;

impl Provider for SettingsPages {
    fn id(&self) -> &'static str {
        ID
    }

    fn catalog(&self) -> Vec<SearchItem> {
        PAGES
            .iter()
            .map(|&(page, title, keywords)| {
                SearchItem::new(item_id(ID, page), title)
                    .subtitle("Settings")
                    .keywords(keywords)
                    .icon(Icon::App(SETTINGS_APP.into()))
                    .action(
                        "Open Settings",
                        Command::OpenUrl(format!("ms-settings:{page}")),
                    )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_and_titles_are_unique() {
        let mut pages: Vec<_> = PAGES.iter().map(|(page, _, _)| *page).collect();
        let mut titles: Vec<_> = PAGES.iter().map(|(_, title, _)| *title).collect();
        pages.sort_unstable();
        titles.sort_unstable();
        let (page_count, title_count) = (pages.len(), titles.len());
        pages.dedup();
        titles.dedup();
        assert_eq!((pages.len(), titles.len()), (page_count, title_count));
        assert!(PAGES.iter().all(|(page, _, _)| {
            page.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        }));
    }
}
