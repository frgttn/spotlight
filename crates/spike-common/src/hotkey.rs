//! System-wide hotkey via `RegisterHotKey` on a dedicated message-loop thread.
//!
//! Registering against no window makes Windows post `WM_HOTKEY` to the
//! registering thread's queue, so the hotkey doesn't depend on how the UI
//! framework runs its own message loop.

use std::{sync::mpsc, thread, time::Instant};

use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, VK_SPACE,
    },
    WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY},
};

/// Tried in order; the first one Windows accepts wins.
const CANDIDATES: &[(HOT_KEY_MODIFIERS, &str)] = &[
    (MOD_ALT, "Alt+Space"),
    (
        HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_SHIFT.0),
        "Ctrl+Shift+Space",
    ),
];

/// Starts the hotkey thread. Each press sends the instant `WM_HOTKEY` was
/// dequeued, which is the start point for the hotkey-to-visible measurement.
///
/// Returns the label of the registered combination, or an error if none of the
/// candidates could be registered (usually because another app owns them).
pub fn spawn(presses: async_channel::Sender<Instant>) -> Result<&'static str, String> {
    let (registered_tx, registered_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || unsafe {
            let registered = CANDIDATES
                .iter()
                .enumerate()
                .find_map(|(id, (mods, label))| {
                    RegisterHotKey(None, id as i32 + 1, *mods | MOD_NOREPEAT, VK_SPACE.0 as u32)
                        .ok()
                        .map(|_| *label)
                });
            let Some(label) = registered else {
                let tried: Vec<_> = CANDIDATES.iter().map(|(_, label)| *label).collect();
                let _ = registered_tx.send(Err(format!("could not register any of {tried:?}")));
                return;
            };
            let _ = registered_tx.send(Ok(label));

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_HOTKEY && presses.send_blocking(Instant::now()).is_err() {
                    break;
                }
            }
        })
        .map_err(|err| err.to_string())?;

    registered_rx
        .recv()
        .unwrap_or_else(|_| Err("hotkey thread exited during registration".into()))
}
