//! Snippet keywords typed in any app become the snippet: `;addr` is
//! backspaced away and the address pasted in its place, the caret at its
//! `{cursor}`.
//!
//! A low-level keyboard hook on a thread of its own turns each key into the
//! characters it types (with the layout of the app in front) and keeps the
//! last few ([`Typed`]). It only reads keys, never holds them back, and does
//! no more than that: Windows drops hooks that are slow. A keyword at the end
//! goes to a second thread, which replaces it through the clipboard and puts
//! the clipboard back. Clicks, arrow keys and switching windows make it forget
//! what was typed, since the caret may have moved.
//!
//! The hooks are installed only while there are keywords to look for. Keys
//! Spotlight sends itself, keys in Spotlight's own windows, keys in password
//! fields and keys in the apps the user excluded are never expanded.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use spotlight_core::snippet::{self, Inputs, Typed};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{
        Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx},
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
    UI::{
        Accessibility::{CUIAutomation, IUIAutomation},
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetKeyState, GetKeyboardLayout, ToUnicodeEx, VIRTUAL_KEY, VK_BACK,
            VK_CAPITAL, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
            VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION,
            HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE, PeekMessageW,
            PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
            WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_RBUTTONDOWN,
            WM_SYSKEYDOWN, WM_USER, WM_XBUTTONDOWN,
        },
    },
};

use crate::{
    clipboard::{self, Content, Snapshot},
    links, selection,
};

/// Posted to the hook thread when the keywords changed.
const WM_KEYWORDS: u32 = WM_APP + 1;
/// How long the app gets to read the pasted snippet before the clipboard is
/// put back. Too short and it pastes what was there before.
const RESTORE_DELAY: Duration = Duration::from_millis(300);

/// The snippets to expand: keyword and text.
static SNIPPETS: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());
/// Programs (paths) where keywords don't expand.
static EXCLUDED: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// Set while a keyword is being replaced: keys typed meanwhile aren't kept.
static BUSY: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// What was typed, on the hook thread.
    static TYPED: RefCell<Typed> = RefCell::new(Typed::default());
    /// The window it was typed in.
    static TYPED_IN: Cell<isize> = const { Cell::new(0) };
    /// Where a keyword found goes to be replaced.
    static REPLACER: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
}

/// A keyword to replace: typed in `window`, `keyword_chars` long.
struct Job {
    window: isize,
    keyword_chars: usize,
    text: String,
}

/// Talks to the expander's threads.
#[derive(Clone)]
pub struct Service {
    thread_id: u32,
}

impl Service {
    /// The snippets to expand, as keyword and text. None: the hooks go.
    pub fn set_snippets(&self, snippets: Vec<(String, String)>) {
        if let Ok(mut current) = SNIPPETS.write() {
            *current = snippets;
        }
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_KEYWORDS, WPARAM(0), LPARAM(0));
        }
    }

    /// The programs (paths) where keywords don't expand.
    pub fn set_excluded(&self, programs: Vec<String>) {
        if let Ok(mut current) = EXCLUDED.write() {
            *current = programs;
        }
    }
}

/// Starts the expander, with no snippets yet. `owner` (a window of ours, on
/// a thread that handles messages) holds the clipboard while a snippet is
/// pasted.
pub fn spawn(owner: isize) -> Result<Service, String> {
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
    thread::Builder::new()
        .name("snippet-replace".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            while let Ok(job) = jobs_rx.recv() {
                BUSY.store(true, Ordering::Release);
                if let Err(err) = replace(&job, owner) {
                    eprintln!("[expander] {err}");
                }
                BUSY.store(false, Ordering::Release);
            }
        })
        .map_err(|err| err.to_string())?;

    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("snippet-keywords".into())
        .spawn(move || unsafe {
            // Create the message queue before anyone posts to it.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
            REPLACER.with(|replacer| *replacer.borrow_mut() = Some(jobs_tx));
            let _ = ready_tx.send(GetCurrentThreadId());

            let mut hooks: Option<(HHOOK, HHOOK)> = None;
            // The hooks are called from this loop, too.
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message != WM_KEYWORDS {
                    continue;
                }
                let wanted = SNIPPETS.read().is_ok_and(|snippets| !snippets.is_empty());
                match (wanted, hooks) {
                    (true, None) => hooks = install(),
                    (false, Some((keyboard, mouse))) => {
                        let _ = UnhookWindowsHookEx(keyboard);
                        let _ = UnhookWindowsHookEx(mouse);
                        hooks = None;
                        TYPED.with(|typed| typed.borrow_mut().clear());
                    }
                    _ => {}
                }
            }
        })
        .map_err(|err| err.to_string())?;
    let thread_id = ready_rx
        .recv()
        .map_err(|_| "the expander thread exited at start".to_owned())?;
    Ok(Service { thread_id })
}

/// The keyboard and mouse hooks, or `None` (and why, on stderr).
unsafe fn install() -> Option<(HHOOK, HHOOK)> {
    unsafe {
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), None, 0)
            .inspect_err(|err| eprintln!("[expander] no keyboard hook: {err}"))
            .ok()?;
        match SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), None, 0) {
            Ok(mouse) => Some((keyboard, mouse)),
            Err(err) => {
                eprintln!("[expander] no mouse hook: {err}");
                let _ = UnhookWindowsHookEx(keyboard);
                None
            }
        }
    }
}

unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if down && event.flags & LLKHF_INJECTED != LLKHF_INJECTED {
                key_down(event.vkCode, event.scanCode);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// A click may move the caret: what was typed no longer ends at it.
unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32
            && matches!(
                wparam.0 as u32,
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
            )
        {
            TYPED.with(|typed| typed.borrow_mut().clear());
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// Keeps what `vk` typed and looks for a keyword at the end.
fn key_down(vk: u32, scan: u32) {
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    TYPED.with(|typed| {
        let mut typed = typed.borrow_mut();
        if TYPED_IN.replace(foreground) != foreground || BUSY.load(Ordering::Acquire) {
            typed.clear();
        }
        if BUSY.load(Ordering::Acquire) || is_modifier(vk) {
            return;
        }
        let held = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        let (ctrl, alt) = (held(VK_CONTROL), held(VK_MENU));
        // Ctrl or Alt alone are shortcuts; both together are AltGr, which types.
        if held(VK_LWIN) || held(VK_RWIN) || ctrl != alt {
            typed.clear();
            return;
        }
        if vk == u32::from(VK_BACK.0) {
            typed.backspace();
            return;
        }
        match translate(vk, scan, foreground) {
            Typing::Text(text) if text.chars().all(|ch| !ch.is_control()) => {
                text.chars().for_each(|ch| typed.push(ch));
            }
            // A dead key (´) types nothing yet; the next key types with it.
            Typing::Dead => return,
            // Enter, Tab, arrows, Escape…: the caret may be elsewhere.
            _ => {
                typed.clear();
                return;
            }
        }
        let Ok(snippets) = SNIPPETS.read() else {
            return;
        };
        let Some(keyword) =
            typed.keyword_at_end(snippets.iter().map(|(keyword, _)| keyword.as_str()))
        else {
            return;
        };
        if is_ours(foreground) {
            return;
        }
        let Some((_, text)) = snippets.iter().find(|(known, _)| known == keyword) else {
            return;
        };
        let job = Job {
            window: foreground,
            keyword_chars: keyword.chars().count(),
            text: text.clone(),
        };
        typed.clear();
        REPLACER.with(|replacer| {
            if let Some(replacer) = replacer.borrow().as_ref() {
                let _ = replacer.send(job);
            }
        });
    });
}

enum Typing {
    Text(String),
    Dead,
    Nothing,
}

/// What `vk` types in `window`'s keyboard layout, with the modifiers held now.
fn translate(vk: u32, scan: u32, window: isize) -> Typing {
    let mut state = [0u8; 256];
    for key in [
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
    ] {
        if unsafe { GetAsyncKeyState(key.0 as i32) } < 0 {
            state[key.0 as usize] = 0x80;
        }
    }
    if unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0 {
        state[VK_CAPITAL.0 as usize] = 0x01;
    }
    let mut buffer = [0u16; 8];
    let written = unsafe {
        let thread = GetWindowThreadProcessId(HWND(window as *mut c_void), None);
        let layout = GetKeyboardLayout(thread);
        // 0x4: leave the keyboard's dead-key state as it is, so the app
        // still gets the accent.
        ToUnicodeEx(vk, scan, &state, &mut buffer, 0x4, Some(layout))
    };
    match written {
        ..0 => Typing::Dead,
        0 => Typing::Nothing,
        len => Typing::Text(String::from_utf16_lossy(&buffer[..len as usize])),
    }
}

fn is_modifier(vk: u32) -> bool {
    [
        VK_SHIFT,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
        VK_CAPITAL,
    ]
    .iter()
    .any(|key| u32::from(key.0) == vk)
}

/// Spotlight's own windows (the launcher, Settings): a keyword typed there
/// is being edited, not used.
fn is_ours(window: isize) -> bool {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(HWND(window as *mut c_void), Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

/// Backspaces the keyword away and pastes the snippet, then puts the
/// clipboard back.
fn replace(job: &Job, owner: isize) -> Result<(), String> {
    selection::wait_for_modifiers();
    if unsafe { GetForegroundWindow() }.0 as isize != job.window {
        return Err("another window came to the front; not expanding".into());
    }
    if is_excluded(job.window) || is_password_field() {
        return Ok(());
    }
    let snapshot =
        Snapshot::take(owner).ok_or("the clipboard is busy or too large to set aside")?;
    clipboard::set_borrowed(true);
    let result = paste(job, owner);
    // Only once the app had time to read it.
    thread::sleep(RESTORE_DELAY);
    if let Err(err) = snapshot.restore(owner) {
        eprintln!("[expander] could not put the clipboard back: {err}");
    }
    clipboard::set_borrowed(false);
    result
}

fn paste(job: &Job, owner: isize) -> Result<(), String> {
    let copied = snippet::needs_clipboard(&job.text)
        .then(|| clipboard::text(owner))
        .flatten();
    let moment = links::moment();
    let expansion = snippet::expand(
        &job.text,
        &Inputs {
            moment: &moment,
            clipboard: copied.as_deref(),
            values: &[],
        },
    );
    let text = snippet::normalize_text(&expansion.text).replace('\n', "\r\n");
    clipboard::write(owner, &Content::Text(text))?;
    // Windows blocks input into apps running as administrator.
    if !clipboard::press(VK_BACK, job.keyword_chars) || !clipboard::press_ctrl(VK_V) {
        return Err("Windows blocked the keys (the app runs as administrator?)".into());
    }
    let caret_back = expansion.moves_back();
    if caret_back > 0 && !clipboard::press(VK_LEFT, caret_back) {
        return Err("Windows blocked Left".into());
    }
    Ok(())
}

const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(b'V' as u16);
const VK_LEFT: VIRTUAL_KEY = VIRTUAL_KEY(0x25);

/// Whether `window` belongs to an app the user excluded.
fn is_excluded(window: isize) -> bool {
    let Some(program) = clipboard::window_program(window) else {
        return false;
    };
    EXCLUDED.read().is_ok_and(|excluded| {
        excluded
            .iter()
            .any(|path| path.eq_ignore_ascii_case(&program))
    })
}

/// Whether the control with the keyboard hides what's typed in it.
fn is_password_field() -> bool {
    unsafe {
        let Ok(automation) =
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        automation
            .GetFocusedElement()
            .and_then(|focused| focused.CurrentIsPassword())
            .is_ok_and(|password| password.as_bool())
    }
}
