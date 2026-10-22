//! Notification-area icon: open the launcher, toggle start at login, quit.
//!
//! `tray-icon` creates a hidden window on the calling thread, so this must run
//! on GPUI's main thread, whose message loop then dispatches the tray messages.

use spotlight_platform_win::autostart;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

pub enum Command {
    Open,
    ToggleAutostart,
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    autostart: CheckMenuItem,
}

impl Tray {
    /// Shows the tray icon. Menu picks and left clicks arrive on `commands`.
    pub fn create(hotkey: &str, commands: async_channel::Sender<Command>) -> Result<Self, String> {
        let open = MenuItem::new("Open Spotlight", true, None);
        let autostart = CheckMenuItem::new("Start at login", true, autostart::is_enabled(), None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &open,
            &PredefinedMenuItem::separator(),
            &autostart,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|err| err.to_string())?;

        let ids: [(MenuId, fn() -> Command); 3] = [
            (open.id().clone(), || Command::Open),
            (autostart.id().clone(), || Command::ToggleAutostart),
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
            .with_tooltip(format!("Spotlight ({hotkey})"))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            _icon: icon,
            autostart,
        })
    }

    /// Flips start at login and shows the resulting state, which stays the old
    /// one if the registry write failed.
    pub fn toggle_autostart(&self) {
        if let Err(err) = autostart::set_enabled(!autostart::is_enabled()) {
            eprintln!("[tray] could not change start at login: {err}");
        }
        self.autostart.set_checked(autostart::is_enabled());
    }
}

/// A white magnifier on a blue disc, drawn at 32 px with antialiased edges.
/// Readable on both light and dark taskbars.
fn icon() -> Result<Icon, String> {
    const SIZE: u32 = 32;
    const BLUE: [f32; 3] = [52., 101., 235.];
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Coverage from a signed distance: 1 inside, 0 outside, a 1 px ramp between.
            let cover = |distance: f32| (0.5 - distance).clamp(0., 1.);

            let disc = cover((px - 16.).hypot(py - 16.) - 15.5);
            let ring = cover(((px - 14.).hypot(py - 14.) - 6.5).abs() - 1.75);
            let handle = cover(segment_distance((px, py), (19., 19.), (24.5, 24.5)) - 2.);
            let glyph = ring.max(handle);

            let channel = |blue: f32| (blue + (255. - blue) * glyph).round() as u8;
            rgba.extend([
                channel(BLUE[0]),
                channel(BLUE[1]),
                channel(BLUE[2]),
                (disc * 255.).round() as u8,
            ]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).map_err(|err| err.to_string())
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0., 1.);
    (p.0 - (a.0 + t * abx)).hypot(p.1 - (a.1 + t * aby))
}
