//! The text selected in another app, for commands that work on it.
//!
//! First UI Automation: the focused control's `TextPattern` selection, which
//! leaves the clipboard alone. Apps that don't expose it (some Electron apps,
//! custom-drawn editors) are asked with Ctrl+C instead: the clipboard is
//! copied aside, the selection copied and read, and the clipboard put back.
//! The clipboard history ignores all of that.
//!
//! The selection can only be read while its app has focus, so this first
//! hands the foreground back to that app.

use std::{
    ffi::c_void,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::HWND,
    System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
    UI::{
        Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
        },
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId},
    },
};

use crate::clipboard::{self, Snapshot};

/// What reading the selection found.
pub type Outcome = Result<Option<String>, String>;

/// How long an app gets to put its selection on the clipboard after Ctrl+C.
const COPY_TIMEOUT: Duration = Duration::from_millis(400);
/// Selections longer than this (characters) are cut.
const MAX_CHARS: usize = 1_000_000;

/// Window classes of terminals, where Ctrl+C stops the running program
/// rather than copying.
const TERMINAL_CLASSES: &[&str] = &[
    "ConsoleWindowClass",
    "CASCADIA_HOSTING_WINDOW_CLASS",
    "VirtualConsoleClass",
    "PuTTY",
    "mintty",
    "org.wezfurlong.wezterm",
];
/// Terminal programs by file name, for those with a generic window class.
const TERMINAL_PROGRAMS: &[&str] = &[
    "windowsterminal",
    "wezterm-gui",
    "alacritty",
    "mintty",
    "putty",
    "conemu64",
    "conemu",
    "kitty",
    "hyper",
    "tabby",
    "warp",
];

/// Reads the text selected in `window`, the window that was active before
/// the launcher. Call it while Spotlight is still the foreground app, which
/// lets it give the foreground back. With `copy`, apps that don't share
/// their selection are sent Ctrl+C; `owner` (a window of ours) then holds
/// the clipboard while it's put back.
///
/// The outcome arrives on the returned channel: the text, `None` when
/// nothing is selected, or why it couldn't be read.
pub fn spawn(window: isize, owner: isize, copy: bool) -> async_channel::Receiver<Outcome> {
    let (tx, rx) = async_channel::bounded(1);
    if !clipboard::bring_to_front(window) {
        let _ = tx.try_send(Err("There's no app to read a selection from".into()));
        return rx;
    }
    thread::spawn(move || {
        let outcome = read(window, owner, copy);
        if let Err(err) = &outcome {
            eprintln!("[selection] {err}");
        }
        let _ = tx.send_blocking(outcome);
    });
    rx
}

fn read(window: isize, owner: isize, copy: bool) -> Outcome {
    if !clipboard::wait_for_focus(window) {
        return Err("The app didn't come back to the front".into());
    }
    match from_automation(window) {
        Ok(Some(text)) => return Ok(Some(text)),
        Ok(None) => {}
        Err(err) => eprintln!("[selection] UI Automation: {err}"),
    }
    if !copy || is_terminal(window) {
        return Ok(None);
    }
    by_copying(owner)
}

/// The selection of the focused control, through its `TextPattern`.
/// `Ok(None)` when it has none or doesn't say.
fn from_automation(window: isize) -> Result<Option<String>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|err| err.to_string())?;
        let focused = automation
            .GetFocusedElement()
            .map_err(|err| err.to_string())?;
        // Only the app the launcher came from; never Spotlight itself.
        let mut pid = 0;
        GetWindowThreadProcessId(HWND(window as *mut c_void), Some(&mut pid));
        if focused.CurrentProcessId().ok() != Some(pid as i32) {
            return Ok(None);
        }
        let Ok(pattern) =
            focused.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
        else {
            return Ok(None);
        };
        let ranges = pattern.GetSelection().map_err(|err| err.to_string())?;
        let mut text = String::new();
        for ix in 0..ranges.Length().unwrap_or(0) {
            if let Ok(range) = ranges.GetElement(ix)
                && let Ok(part) = range.GetText(MAX_CHARS as i32)
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part.to_string());
            }
        }
        Ok(non_empty(text))
    }
}

/// Waits (up to a second) until no modifier key is held: after an item
/// hotkey (Ctrl+Alt+S) the user may still hold Alt, and Ctrl+C would become
/// Ctrl+Alt+C.
fn wait_for_modifiers() {
    let held = || {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) } < 0)
    };
    let started = Instant::now();
    while held() && started.elapsed() < Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(10));
    }
}

/// Copies the selection with Ctrl+C and puts the clipboard back.
fn by_copying(owner: isize) -> Outcome {
    wait_for_modifiers();
    let snapshot =
        Snapshot::take(owner).ok_or("The clipboard is busy or too large to set aside")?;
    clipboard::set_borrowed(true);
    let before = clipboard::sequence();
    let result = if clipboard::press_ctrl(VIRTUAL_KEY(b'C' as u16)) {
        let started = Instant::now();
        while clipboard::sequence() == before && started.elapsed() < COPY_TIMEOUT {
            thread::sleep(Duration::from_millis(10));
        }
        // No change: nothing was selected (most apps copy nothing then).
        if clipboard::sequence() == before {
            Ok(None)
        } else {
            // The app may still be adding formats.
            thread::sleep(Duration::from_millis(30));
            Ok(clipboard::text(owner).and_then(non_empty))
        }
    } else {
        Err("Windows blocked Ctrl+C (the app runs as administrator)".into())
    };
    if clipboard::sequence() != before
        && let Err(err) = snapshot.restore(owner)
    {
        eprintln!("[selection] could not put the clipboard back: {err}");
    }
    clipboard::set_borrowed(false);
    result
}

/// Terminals take Ctrl+C as "stop", so they're never sent it.
fn is_terminal(window: isize) -> bool {
    let hwnd = HWND(window as *mut c_void);
    let mut class = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    let class = String::from_utf16_lossy(&class[..len]);
    if TERMINAL_CLASSES.contains(&class.as_str()) {
        return true;
    }
    let program = clipboard::window_program(window)
        .and_then(|path| {
            Path::new(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_lowercase())
        })
        .unwrap_or_default();
    TERMINAL_PROGRAMS.contains(&program.as_str())
}

fn non_empty(text: String) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_CHARS).collect())
}
