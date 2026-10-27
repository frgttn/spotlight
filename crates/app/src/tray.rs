//! Notification-area icon: open the launcher or settings, toggle start at login,
//! update, quit.
//!
//! `tray-icon` creates a hidden window on the calling thread, so this must run
//! on GPUI's main thread, whose message loop then dispatches the tray messages.

use spotlight_core::update::Status as UpdateStatus;
use spotlight_platform_win::autostart;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

pub enum Command {
    Open,
    Settings,
    ToggleAutostart,
    /// Check for updates, or restart into the downloaded one.
    Update,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    autostart: CheckMenuItem,
    update: MenuItem,
}

impl Tray {
    /// Shows the tray icon. Menu picks and left clicks arrive on `commands`.
    pub fn create(hotkey: &str, commands: async_channel::Sender<Command>) -> Result<Self, String> {
        let open = MenuItem::new("Open Spotlight", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let autostart = CheckMenuItem::new("Start at login", true, autostart::is_enabled(), None);
        let update = MenuItem::new("Check for Updates", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &open,
            &settings,
            &PredefinedMenuItem::separator(),
            &autostart,
            &update,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|err| err.to_string())?;

        let ids: [(MenuId, fn() -> Command); 5] = [
            (open.id().clone(), || Command::Open),
            (settings.id().clone(), || Command::Settings),
            (autostart.id().clone(), || Command::ToggleAutostart),
            (update.id().clone(), || Command::Update),
            (quit.id().clone(), || Command::Quit),
        ];
        let menu_commands = commands.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, command)) = ids.iter().find(|(id, _)| *id == event.id) {
                let _ = menu_commands.send_blocking(command());
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = commands.send_blocking(Command::Open);
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_icon(icon()?)
            .with_tooltip(tooltip(hotkey))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            icon,
            autostart,
            update,
        })
    }

    pub fn set_hotkey(&self, hotkey: &str) {
        let _ = self.icon.set_tooltip(Some(tooltip(hotkey)));
    }

    /// Flips start at login and shows the resulting state, which stays the old
    /// one if the registry write failed.
    pub fn toggle_autostart(&self) {
        if let Err(err) = autostart::set_enabled(!autostart::is_enabled()) {
            eprintln!("[tray] could not change start at login: {err}");
        }
        self.sync_autostart();
    }

    /// Updates the check mark after start at login changed elsewhere (the settings window).
    pub fn sync_autostart(&self) {
        self.autostart.set_checked(autostart::is_enabled());
    }

    /// Shows what the update item does now. Off while checking or
    /// downloading, and for a copy the installer didn't put there.
    pub fn set_update_status(&self, status: &UpdateStatus) {
        let text = match status {
            UpdateStatus::Checking => "Checking for Updates…".to_owned(),
            UpdateStatus::Downloading(version) => format!("Downloading v{version}…"),
            UpdateStatus::Ready(version) => format!("Restart to Update to v{version}"),
            _ => "Check for Updates".to_owned(),
        };
        self.update.set_text(text);
        self.update
            .set_enabled(!status.busy() && *status != UpdateStatus::Unmanaged);
    }
}

fn tooltip(hotkey: &str) -> String {
    format!("Spotlight ({hotkey})")
}

/// The logo (two interlocked half rings, the same geometry as `assets/logo-white.svg`
/// and `assets/logo-black.svg`) on a transparent background, drawn at 32 px with
/// 4x4 supersampling. White on a dark taskbar, black on a light one.
fn icon() -> Result<Icon, String> {
    const SIZE: u32 = 32;
    const SAMPLES: u32 = 4;
    // Logo coordinates are on a 100 x 100 grid; this many pixels per unit.
    const SCALE: f32 = 0.38;
    let mark: [u8; 3] = if spotlight_platform_win::system::taskbar_dark() {
        [255, 255, 255]
    } else {
        [0, 0, 0]
    };

    // Left half ring around (50, 36.75) and right half ring around (50, 63.25),
    // both 27.5 outside and 13.25 inside (the SVG's two subpaths, as rendered).
    let in_mark = |u: f32, v: f32| {
        let ring = |cy: f32| {
            let d = (u - 50.).hypot(v - cy);
            (13.25..=27.5).contains(&d)
        };
        (u <= 50. && ring(36.75)) || (u >= 50. && ring(63.25))
    };

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut covered = 0u32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    if in_mark(50. + (px - 16.) / SCALE, 50. + (py - 16.) / SCALE) {
                        covered += 1;
                    }
                }
            }
            let alpha = covered as f32 / (SAMPLES * SAMPLES) as f32;
            rgba.extend([mark[0], mark[1], mark[2], (alpha * 255.).round() as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).map_err(|err| err.to_string())
}
