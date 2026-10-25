//! Snippet keywords typed in any app become the snippet: `;addr` is
//! backspaced away and the address pasted in its place, the caret at its
//! `{cursor}`.
//!
//! A low-level keyboard hook on a thread of its own turns each key into the
//! characters it types (with the layout of the app in front) and keeps the
//! last few ([`Typed`]), and does no more than that: Windows drops hooks that
//! are slow. A keyword at the end goes to a second thread, which replaces it
//! through the clipboard and puts the clipboard back. Clicks, arrow keys and
//! switching windows make it forget what was typed, since the caret may have
//! moved.
//!
//! Keys typed while a keyword is being replaced are held back and sent again
//! once the snippet is in, so they land after it instead of in the middle.
//! A snippet with arguments asks the launcher for a form instead (see
//! [`Ask`]); the keys held meanwhile go to the form.
//!
//! The hooks are installed only while there are keywords to look for. Keys
//! Spotlight sends itself, keys in Spotlight's own windows, keys in password
//! fields and keys in the apps the user excluded are never expanded.

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use spotlight_core::snippet::{self, Inputs, Typed};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, SendInput,
};
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
            HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, MSG, PM_NOREMOVE, PeekMessageW,
            PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
            WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
            WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_XBUTTONDOWN,
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

/// How long the launcher gets to show a snippet's form before the keys held
/// for it are sent anyway.
const FORM_WAIT: Duration = Duration::from_millis(1500);
/// Keys are never held longer than this, whatever goes wrong.
const HOLD_LIMIT_MS: u64 = 4000;

/// A snippet to expand when its keyword is typed.
#[derive(Clone, Debug)]
pub struct Keyword {
    pub keyword: String,
    pub name: String,
    /// With the snippets it includes already in it.
    pub text: String,
}

/// A keyword whose snippet has arguments was typed (and deleted) in
/// `window`: the launcher asks for them, then calls [`Service::paste`].
#[derive(Clone, Debug)]
pub struct Ask {
    pub window: isize,
    pub name: String,
    pub text: String,
}

/// The snippets to expand.
static SNIPPETS: RwLock<Vec<Keyword>> = RwLock::new(Vec::new());
/// Programs (paths) where keywords don't expand.
static EXCLUDED: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// Set while a keyword is being replaced: keys typed meanwhile aren't kept.
static BUSY: AtomicBool = AtomicBool::new(false);
/// When keys started being held (`GetTickCount64`), or 0 when they aren't.
static HOLDING_SINCE: AtomicU64 = AtomicU64::new(0);
static HELD: Mutex<Held> = Mutex::new(Held {
    events: Vec::new(),
    down: Vec::new(),
});

/// Keys held back while a keyword is replaced.
struct Held {
    /// In order: virtual key, scan code, extended, key up.
    events: Vec<(u16, u16, bool, bool)>,
    /// Keys whose press was held, so their release is held too. A release
    /// of a key pressed before goes through.
    down: Vec<u32>,
}

thread_local! {
    /// What was typed, on the hook thread.
    static TYPED: RefCell<Typed> = RefCell::new(Typed::default());
    /// The window it was typed in.
    static TYPED_IN: Cell<isize> = const { Cell::new(0) };
    /// Where a keyword found goes to be replaced.
    static REPLACER: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
}

/// A keyword to replace: typed in `window`, `keyword_chars` long. With
/// `keyword_chars` 0 it's a snippet whose form was filled, to paste once
/// `window` is back in front.
struct Job {
    window: isize,
    keyword_chars: usize,
    name: String,
    text: String,
}

/// Talks to the expander's threads.
#[derive(Clone)]
pub struct Service {
    thread_id: u32,
    jobs: mpsc::Sender<Job>,
    form_shown: mpsc::Sender<()>,
}

impl Service {
    /// The snippets to expand. None: the hooks go.
    pub fn set_snippets(&self, snippets: Vec<Keyword>) {
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

    /// The form [`Ask`] asked for is up and has the keyboard: the keys held
    /// since the keyword can go to it.
    pub fn form_shown(&self) {
        let _ = self.form_shown.send(());
    }

    /// Pastes `text` (a snippet with its arguments filled in) into `window`
    /// once it's in front, as a typed keyword is, and puts the clipboard back.
    pub fn paste(&self, window: isize, text: String) {
        let _ = self.jobs.send(Job {
            window,
            keyword_chars: 0,
            name: String::new(),
            text,
        });
    }
}

/// Starts the expander, with no snippets yet. `owner` (a window of ours, on
/// a thread that handles messages) holds the clipboard while a snippet is
/// pasted. Snippets with arguments go to `asks`.
pub fn spawn(owner: isize, asks: async_channel::Sender<Ask>) -> Result<Service, String> {
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
    let (shown_tx, shown_rx) = mpsc::channel::<()>();
    let hook_jobs = jobs_tx.clone();
    thread::Builder::new()
        .name("snippet-replace".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            while let Ok(job) = jobs_rx.recv() {
                BUSY.store(true, Ordering::Release);
                let result = if job.keyword_chars == 0 {
                    paste_filled(&job, owner)
                } else {
                    replace(&job, owner, &asks, &shown_rx)
                };
                // Whatever happened, the keys held for it go out.
                release_held();
                if let Err(err) = result {
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
            REPLACER.with(|replacer| *replacer.borrow_mut() = Some(hook_jobs));
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
    Ok(Service {
        thread_id,
        jobs: jobs_tx,
        form_shown: shown_tx,
    })
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
            let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            if event.flags & LLKHF_INJECTED != LLKHF_INJECTED {
                if (down || up) && hold(event, up) {
                    return LRESULT(1);
                }
                if down {
                    key_down(event.vkCode, event.scanCode);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// Holds the key back while a keyword is replaced. `true` when it's held.
fn hold(event: &KBDLLHOOKSTRUCT, up: bool) -> bool {
    let since = HOLDING_SINCE.load(Ordering::Acquire);
    if since == 0 || unsafe { GetTickCount64() }.saturating_sub(since) > HOLD_LIMIT_MS {
        return false;
    }
    let Ok(mut held) = HELD.lock() else {
        return false;
    };
    if up {
        let Some(ix) = held.down.iter().position(|&vk| vk == event.vkCode) else {
            return false;
        };
        held.down.remove(ix);
    } else if !held.down.contains(&event.vkCode) {
        held.down.push(event.vkCode);
    }
    let extended = event.flags & LLKHF_EXTENDED == LLKHF_EXTENDED;
    held.events
        .push((event.vkCode as u16, event.scanCode as u16, extended, up));
    true
}

/// Stops holding keys and sends the ones held, in order, to whatever has
/// the keyboard now.
fn release_held() {
    let events = match HELD.lock() {
        Ok(mut held) => {
            HOLDING_SINCE.store(0, Ordering::Release);
            held.down.clear();
            std::mem::take(&mut held.events)
        }
        Err(_) => {
            HOLDING_SINCE.store(0, Ordering::Release);
            return;
        }
    };
    if events.is_empty() {
        return;
    }
    let inputs: Vec<INPUT> = events
        .into_iter()
        .map(|(vk, scan, extended, up)| {
            let mut flags = KEYBD_EVENT_FLAGS(0);
            if up {
                flags |= KEYEVENTF_KEYUP;
            }
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: scan,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            }
        })
        .collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        eprintln!("[expander] Windows blocked the keys typed during the expansion");
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
            typed.keyword_at_end(snippets.iter().map(|snippet| snippet.keyword.as_str()))
        else {
            return;
        };
        if is_ours(foreground) {
            return;
        }
        let Some(snippet) = snippets.iter().find(|snippet| snippet.keyword == keyword) else {
            return;
        };
        let job = Job {
            window: foreground,
            keyword_chars: keyword.chars().count(),
            name: snippet.name.clone(),
            text: snippet.text.clone(),
        };
        typed.clear();
        // From the next key on, until the snippet is in.
        HOLDING_SINCE.store(unsafe { GetTickCount64() }.max(1), Ordering::Release);
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
/// clipboard back. A snippet with arguments goes to the launcher instead,
/// once the keyword is gone.
fn replace(
    job: &Job,
    owner: isize,
    asks: &async_channel::Sender<Ask>,
    form_shown: &mpsc::Receiver<()>,
) -> Result<(), String> {
    selection::wait_for_modifiers();
    if unsafe { GetForegroundWindow() }.0 as isize != job.window {
        return Err("another window came to the front; not expanding".into());
    }
    if is_excluded(job.window) || is_password_field() {
        return Ok(());
    }
    if !snippet::arguments(&job.text).is_empty() {
        if !clipboard::press(VK_BACK, job.keyword_chars) {
            return Err("Windows blocked the keys (the app runs as administrator?)".into());
        }
        while form_shown.try_recv().is_ok() {}
        let ask = Ask {
            window: job.window,
            name: job.name.clone(),
            text: job.text.clone(),
        };
        if asks.send_blocking(ask).is_ok() {
            let _ = form_shown.recv_timeout(FORM_WAIT);
        }
        return Ok(());
    }
    with_clipboard(owner, || paste(job, owner))
}

/// Pastes a snippet whose form was filled into its window, once it's back
/// in front.
fn paste_filled(job: &Job, owner: isize) -> Result<(), String> {
    if !clipboard::wait_for_focus(job.window) {
        return Err("the window didn't come back to the front; not pasting".into());
    }
    with_clipboard(owner, || paste(job, owner))
}

/// Runs `paste` with the clipboard set aside, then puts it back.
fn with_clipboard(owner: isize, paste: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let snapshot =
        Snapshot::take(owner).ok_or("the clipboard is busy or too large to set aside")?;
    clipboard::set_borrowed(true);
    let result = paste();
    // Right after the snippet: the app reads them after it.
    release_held();
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
