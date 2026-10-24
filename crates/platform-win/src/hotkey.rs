//! System-wide hotkeys via `RegisterHotKey` on a dedicated message-loop thread:
//! the launcher's own, plus any number of item hotkeys that run one command.
//!
//! Registering against no window makes Windows post `WM_HOTKEY` to the
//! registering thread's queue, so the hotkey doesn't depend on how the UI
//! framework runs its own message loop. The same thread changes the hotkey
//! and records new ones, because a hotkey can only be unregistered by the
//! thread that registered it, and a low-level hook runs on the thread that
//! installed it.
//!
//! Windows keeps most Win+key combinations for itself, so `RegisterHotKey`
//! refuses them. Such a combination is taken with a low-level keyboard hook
//! instead, which sees keys before Windows does; so is Win+V for the
//! clipboard history (see [`Service::intercept_win_v`]). The hook doesn't see
//! keys while a window of an elevated app is in front, which a registered
//! hotkey does.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt,
    mem::size_of,
    sync::mpsc,
    thread,
    time::Instant,
};

use windows::Win32::{
    Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError, LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS,
            KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
            RegisterHotKey, SendInput, UnregisterHotKey, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE,
            VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
            VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
            PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WM_APP, WM_HOTKEY, WM_KEYDOWN, WM_SYSKEYDOWN, WM_USER,
        },
    },
};

/// A key combination. `vk` is a Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

impl Hotkey {
    /// Tried in order when no hotkey is configured or the configured one is taken.
    pub const DEFAULTS: [Hotkey; 2] = [
        Hotkey {
            ctrl: false,
            alt: true,
            shift: false,
            win: false,
            vk: VK_SPACE,
        },
        Hotkey {
            ctrl: true,
            alt: false,
            shift: true,
            win: false,
            vk: VK_SPACE,
        },
    ];

    /// Builds a hotkey from modifier flags and a key name (`"Space"`, `"K"`,
    /// `"F5"`, `"PageUp"`, `"/"`…, case-insensitive). `None` for keys that
    /// can't be a hotkey and for combinations without a modifier, which
    /// would swallow that key everywhere.
    pub fn new(ctrl: bool, alt: bool, shift: bool, win: bool, key: &str) -> Option<Self> {
        let vk = vk_from_name(key)?;
        (ctrl || alt || shift || win).then_some(Self {
            ctrl,
            alt,
            shift,
            win,
            vk,
        })
    }

    /// Parses the [`Display`](fmt::Display) form, e.g. `"Ctrl+Shift+Space"`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        // "Ctrl++" names the plus key: splitting leaves two empty parts at the end.
        if text.trim_end().ends_with("++") {
            parts.truncate(parts.len() - 2);
            parts.push("+");
        }
        let key = parts.pop()?;
        let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                "win" | "windows" | "super" => win = true,
                _ => return None,
            }
        }
        Self::new(ctrl, alt, shift, win, key)
    }

    fn modifiers(self) -> HOT_KEY_MODIFIERS {
        let mut mods = MOD_NOREPEAT;
        for (on, flag) in [
            (self.ctrl, MOD_CONTROL),
            (self.alt, MOD_ALT),
            (self.shift, MOD_SHIFT),
            (self.win, MOD_WIN),
        ] {
            if on {
                mods |= flag;
            }
        }
        mods
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&key_name(self.vk))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetError {
    /// Another app (or Windows itself) already owns this combination.
    InUse,
    Other(String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::InUse => f.write_str("already used by another app or by Windows"),
            SetError::Other(message) => f.write_str(message),
        }
    }
}

/// What the recorder saw, see [`Service::record`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    Hotkey(Hotkey),
    /// Esc with no modifiers.
    Cancel,
    /// A key pressed without Ctrl, Alt, Shift or Win.
    NeedsModifier,
}

enum Request {
    Set(Hotkey, mpsc::Sender<Result<(), SetError>>),
    SetItem(String, Option<Hotkey>, mpsc::Sender<Result<(), SetError>>),
    Record(async_channel::Sender<Recorded>),
    Resume,
    InterceptWinV(Option<String>, mpsc::Sender<Result<(), String>>),
}

/// Handle to the hotkey thread.
#[derive(Clone)]
pub struct Service {
    thread_id: u32,
    requests: mpsc::Sender<Request>,
}

impl Service {
    /// Switches to `hotkey` and ends recording. On failure the previous hotkey
    /// stays registered. Blocks until the hotkey thread answers, which takes
    /// well under a millisecond.
    pub fn set(&self, hotkey: Hotkey) -> Result<(), SetError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(Request::Set(hotkey, reply_tx));
        reply_rx
            .recv()
            .unwrap_or_else(|_| Err(SetError::Other("the hotkey thread has stopped".into())))
    }

    /// Gives the item `item_id` the hotkey `hotkey`, or takes its hotkey away
    /// (`None`), and ends recording. On failure the item keeps its previous
    /// hotkey. The launcher's own hotkey counts as taken.
    pub fn set_item(&self, item_id: &str, hotkey: Option<Hotkey>) -> Result<(), SetError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(Request::SetItem(item_id.to_owned(), hotkey, reply_tx));
        reply_rx
            .recv()
            .unwrap_or_else(|_| Err(SetError::Other("the hotkey thread has stopped".into())))
    }

    /// Starts recording a shortcut: the hotkey is unregistered and a
    /// low-level keyboard hook reports every key combination pressed anywhere
    /// on `recorded`, swallowing it. The hook sees keys before Windows matches
    /// them against hotkeys, so a combination owned by another app arrives too
    /// (and [`Self::set`] then reports it as [`SetError::InUse`]).
    ///
    /// Recording ends with [`Self::set`] or [`Self::resume`]; until then the
    /// user can't type anywhere else, so callers end it on Esc and when their
    /// window loses focus.
    pub fn record(&self, recorded: async_channel::Sender<Recorded>) {
        self.send(Request::Record(recorded));
    }

    /// Ends recording and re-registers the current hotkey.
    pub fn resume(&self) {
        self.send(Request::Resume);
    }

    /// Makes Win+V run the item `item_id` (its id arrives on `item_presses`,
    /// as for an item hotkey) instead of opening Windows' clipboard history,
    /// or (`None`) gives Win+V back to Windows. Fails while Win+V is the
    /// hotkey of the launcher or of an item.
    pub fn intercept_win_v(&self, item_id: Option<&str>) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(Request::InterceptWinV(item_id.map(str::to_owned), reply_tx));
        reply_rx
            .recv()
            .unwrap_or_else(|_| Err("the hotkey thread has stopped".into()))
    }

    fn send(&self, request: Request) {
        if self.requests.send(request).is_ok() {
            unsafe {
                let _ = PostThreadMessageW(
                    self.thread_id,
                    WM_REQUEST,
                    Default::default(),
                    Default::default(),
                );
            }
        }
    }
}

const WM_REQUEST: u32 = WM_APP + 1;

/// Starts the hotkey thread. Each press of the launcher hotkey sends the
/// instant `WM_HOTKEY` was dequeued (the start point for the hotkey-to-visible
/// measurement) on `presses`; each press of an item hotkey sends that item's
/// id on `item_presses`.
///
/// Tries `preferred` first, then [`Hotkey::DEFAULTS`]. Returns the service and
/// the hotkey that got registered, or an error naming what was tried (usually
/// because other apps own all of them; the service still works for `set`).
pub fn spawn(
    preferred: Option<Hotkey>,
    presses: async_channel::Sender<Instant>,
    item_presses: async_channel::Sender<String>,
) -> Result<(Service, Result<Hotkey, String>), String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (requests_tx, requests_rx) = mpsc::channel::<Request>();
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || unsafe {
            // Create the message queue before anyone posts to it.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);

            let candidates: Vec<Hotkey> = preferred.into_iter().chain(Hotkey::DEFAULTS).collect();
            let mut registrar = Registrar::default();
            let registered = candidates
                .iter()
                .find(|hotkey| registrar.register(**hotkey).is_ok())
                .copied()
                .ok_or_else(|| {
                    let tried: Vec<_> = candidates.iter().map(ToString::to_string).collect();
                    format!("could not register any of {tried:?}")
                });
            let _ = ready_tx.send((GetCurrentThreadId(), registered));
            HOOK_PRESSES.with(|senders| {
                *senders.borrow_mut() = Some((presses.clone(), item_presses.clone()))
            });

            // The low-level hook is called from this loop, too.
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                match msg.message {
                    WM_HOTKEY => {
                        let pressed_at = Instant::now();
                        let id = msg.wParam.0 as i32;
                        let sent = if id <= LAUNCHER_IDS {
                            presses.send_blocking(pressed_at).is_ok()
                        } else if let Some(item) = registrar.item_for(id) {
                            item_presses.send_blocking(item.to_owned()).is_ok()
                        } else {
                            true
                        };
                        if !sent {
                            break;
                        }
                    }
                    WM_REQUEST => {
                        for request in requests_rx.try_iter() {
                            registrar.handle(request);
                        }
                    }
                    _ => {}
                }
            }
            registrar.stop_recording();
            registrar.release_key_hook();
        })
        .map_err(|err| err.to_string())?;

    let (thread_id, registered) = ready_rx
        .recv()
        .map_err(|_| "hotkey thread exited during registration".to_owned())?;
    Ok((
        Service {
            thread_id,
            requests: requests_tx,
        },
        registered,
    ))
}

/// The launcher hotkey alternates between ids 1 and 2; item hotkeys get ids above.
const LAUNCHER_IDS: i32 = 2;

const WIN_V: Hotkey = Hotkey {
    ctrl: false,
    alt: false,
    shift: false,
    win: true,
    vk: b'V' as u16,
};

/// How a hotkey is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    /// With `RegisterHotKey`, under this id.
    Registered(i32),
    /// By the low-level hook (see `hooked_key`).
    Hooked,
}

/// What a hotkey taken by the hook runs.
#[derive(Clone)]
enum Target {
    Launcher,
    Item(String),
}

/// State owned by the hotkey thread.
#[derive(Default)]
struct Registrar {
    current: Option<Hotkey>,
    /// The launcher hotkey is taken by the hook rather than registered.
    current_hooked: bool,
    /// Ids alternate so a new hotkey can be registered before the old one goes.
    id: i32,
    /// While recording the hotkey is unregistered and this hook is installed.
    hook: Option<HHOOK>,
    /// Item id → (binding, hotkey).
    items: HashMap<String, (Binding, Hotkey)>,
    /// Last hotkey id handed to an item.
    last_item_id: i32,
    /// The item Win+V runs (see `Service::intercept_win_v`).
    win_v: Option<String>,
    /// Installed while any hotkey is taken by the hook.
    key_hook: Option<HHOOK>,
}

thread_local! {
    /// Where the hook sends what it records. The hook runs on the hotkey thread.
    static RECORDER: RefCell<Option<async_channel::Sender<Recorded>>> = const { RefCell::new(None) };
    /// Where `hooked_key` reports launcher and item presses.
    static HOOK_PRESSES: RefCell<Option<(async_channel::Sender<Instant>, async_channel::Sender<String>)>> =
        const { RefCell::new(None) };
    /// The hotkeys `hooked_key` takes; rebuilt by `Registrar::update_hooked`.
    static HOOKED: RefCell<Vec<(Hotkey, Target)>> = const { RefCell::new(Vec::new()) };
    /// The key of a hotkey `hooked_key` took: its repeats and release are
    /// swallowed too.
    static SWALLOWED: Cell<Option<u16>> = const { Cell::new(None) };
    /// Bits of `MODIFIER_KEYS` that `hooked_key` saw go down and not up, and
    /// the time of the last key event it saw (see `track_modifiers`).
    static HELD: Cell<(u8, u32)> = const { Cell::new((0, 0)) };
}

impl Registrar {
    /// Registers `hotkey` in addition to the current one and makes it current.
    fn register(&mut self, hotkey: Hotkey) -> Result<(), SetError> {
        if self.held_by_item(hotkey) {
            return Err(SetError::InUse);
        }
        let id = if self.id == 1 { 2 } else { 1 };
        let hooked = match register_hotkey(id, hotkey) {
            Ok(()) => false,
            Err(SetError::InUse) if hotkey.win => {
                hookable(hotkey)?;
                self.ensure_key_hook()?;
                true
            }
            Err(err) => return Err(err),
        };
        if self.current.is_some() && self.hook.is_none() && !self.current_hooked {
            unsafe {
                let _ = UnregisterHotKey(None, self.id);
            }
        }
        self.current = Some(hotkey);
        self.current_hooked = hooked;
        self.id = id;
        self.update_hooked();
        Ok(())
    }

    /// Whether an item, or Win+V, already holds `hotkey`.
    fn held_by_item(&self, hotkey: Hotkey) -> bool {
        self.items.values().any(|&(_, held)| held == hotkey)
            || (self.win_v.is_some() && hotkey == WIN_V)
    }

    fn ensure_key_hook(&mut self) -> Result<(), SetError> {
        if self.key_hook.is_none() {
            let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hooked_key), None, 0) }
                .map_err(|err| {
                    SetError::Other(format!("could not install the keyboard hook: {err}"))
                })?;
            self.key_hook = Some(hook);
        }
        Ok(())
    }

    /// Hands `hooked_key` the hotkeys it takes now, and removes the hook
    /// when there are none.
    fn update_hooked(&mut self) {
        let mut hooked = Vec::new();
        if let (Some(hotkey), true) = (self.current, self.current_hooked) {
            hooked.push((hotkey, Target::Launcher));
        }
        for (item, &(binding, hotkey)) in &self.items {
            if binding == Binding::Hooked {
                hooked.push((hotkey, Target::Item(item.clone())));
            }
        }
        if let Some(item) = &self.win_v {
            hooked.push((WIN_V, Target::Item(item.clone())));
        }
        let empty = hooked.is_empty();
        HOOKED.with(|current| *current.borrow_mut() = hooked);
        if empty {
            self.release_key_hook();
        }
    }

    fn release_key_hook(&mut self) {
        if let Some(hook) = self.key_hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        SWALLOWED.set(None);
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Set(hotkey, reply) => {
                let result = if self.current == Some(hotkey) && self.hook.is_none() {
                    Ok(())
                } else {
                    self.register(hotkey)
                };
                // `register` has registered the new hotkey; only after a
                // failure does recording leave nothing registered.
                if result.is_ok() {
                    self.stop_recording();
                } else {
                    self.resume();
                }
                let _ = reply.send(result);
            }
            Request::SetItem(item, hotkey, reply) => {
                let result = self.set_item(item, hotkey);
                self.resume();
                let _ = reply.send(result);
            }
            Request::Record(recorded) => {
                RECORDER.with(|recorder| *recorder.borrow_mut() = Some(recorded));
                if self.hook.is_none() {
                    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(record_key), None, 0) } {
                        Ok(hook) => {
                            // A hooked launcher hotkey stays: the recording
                            // hook, installed later, sees keys first.
                            if self.current.is_some() && !self.current_hooked {
                                unsafe {
                                    let _ = UnregisterHotKey(None, self.id);
                                }
                            }
                            self.hook = Some(hook);
                        }
                        Err(err) => eprintln!("[hotkey] could not start recording: {err}"),
                    }
                }
            }
            Request::Resume => self.resume(),
            Request::InterceptWinV(item, reply) => {
                let result = match item {
                    Some(_) if self.current == Some(WIN_V) => {
                        Err("Win+V already opens Spotlight".to_owned())
                    }
                    Some(_) if self.items.values().any(|&(_, held)| held == WIN_V) => {
                        Err("Win+V is already the hotkey of a command".to_owned())
                    }
                    Some(item) => self
                        .ensure_key_hook()
                        .map_err(|err| err.to_string())
                        .map(|()| {
                            self.win_v = Some(item);
                        }),
                    None => {
                        self.win_v = None;
                        Ok(())
                    }
                };
                self.update_hooked();
                let _ = reply.send(result);
            }
        }
    }

    fn item_for(&self, id: i32) -> Option<&str> {
        self.items
            .iter()
            .find(|(_, (binding, _))| *binding == Binding::Registered(id))
            .map(|(item, _)| item.as_str())
    }

    fn set_item(&mut self, item: String, hotkey: Option<Hotkey>) -> Result<(), SetError> {
        let previous = self.items.get(&item).copied();
        let Some(hotkey) = hotkey else {
            if let Some((binding, _)) = self.items.remove(&item) {
                unregister(binding);
                self.update_hooked();
            }
            return Ok(());
        };
        if previous.is_some_and(|(_, old)| old == hotkey) {
            return Ok(());
        }
        // While recording the launcher hotkey isn't registered, so Windows
        // wouldn't object to taking it; it would then be lost on resume. And
        // Windows doesn't know about hotkeys the hook takes.
        if self.current == Some(hotkey) || self.held_by_item(hotkey) {
            return Err(SetError::InUse);
        }
        let id = self.last_item_id.max(LAUNCHER_IDS) + 1;
        let binding = match register_hotkey(id, hotkey) {
            Ok(()) => {
                self.last_item_id = id;
                Binding::Registered(id)
            }
            Err(SetError::InUse) if hotkey.win => {
                hookable(hotkey)?;
                self.ensure_key_hook()?;
                Binding::Hooked
            }
            Err(err) => return Err(err),
        };
        if let Some((old, _)) = previous {
            unregister(old);
        }
        self.items.insert(item, (binding, hotkey));
        self.update_hooked();
        Ok(())
    }

    /// Ends recording, if any, and puts the current hotkey back.
    fn resume(&mut self) {
        if self.hook.is_none() {
            return;
        }
        self.stop_recording();
        if let (Some(hotkey), false) = (self.current, self.current_hooked) {
            unsafe {
                if RegisterHotKey(None, self.id, hotkey.modifiers(), hotkey.vk as u32).is_err() {
                    eprintln!("[hotkey] could not re-register {hotkey}");
                    self.current = None;
                }
            }
        }
    }

    fn stop_recording(&mut self) {
        if let Some(hook) = self.hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
        RECORDER.with(|recorder| *recorder.borrow_mut() = None);
    }
}

fn register_hotkey(id: i32, hotkey: Hotkey) -> Result<(), SetError> {
    unsafe {
        if RegisterHotKey(None, id, hotkey.modifiers(), hotkey.vk as u32).is_err() {
            return Err(if GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED {
                SetError::InUse
            } else {
                SetError::Other(windows::core::Error::from_win32().message())
            });
        }
    }
    Ok(())
}

/// Lets go of an item hotkey. One the hook takes goes with the next
/// `update_hooked`.
fn unregister(binding: Binding) {
    if let Binding::Registered(id) = binding {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
}

/// Whether the hook can take `hotkey` from Windows. L with Win held locks
/// the PC whatever the other modifiers and hooks say.
fn hookable(hotkey: Hotkey) -> Result<(), SetError> {
    if hotkey.win && hotkey.vk == u16::from(b'L') {
        return Err(SetError::Other(
            "Windows keeps Win+L for locking the PC".into(),
        ));
    }
    Ok(())
}

/// The hotkey `vk` makes with the modifiers held now.
fn held_with(vk: u16) -> Hotkey {
    let pressed = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
    Hotkey {
        ctrl: pressed(VK_CONTROL),
        alt: pressed(VK_MENU),
        shift: pressed(VK_SHIFT),
        win: pressed(VK_LWIN) || pressed(VK_RWIN),
        vk,
    }
}

/// Low-level keyboard hook used while recording. Modifier keys pass through so
/// their state stays current; any other key press is reported and swallowed.
unsafe extern "system" fn record_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let key_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if code == HC_ACTION as i32 && key_down {
            let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk = event.vkCode as u16;
            if !is_modifier(vk) {
                let hotkey = held_with(vk);
                let recorded = if hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.win {
                    Recorded::Hotkey(hotkey)
                } else if vk == VK_ESCAPE.0 {
                    Recorded::Cancel
                } else {
                    Recorded::NeedsModifier
                };
                let sent = RECORDER.with(|recorder| {
                    recorder
                        .borrow()
                        .as_ref()
                        .is_some_and(|recorder| recorder.try_send(recorded).is_ok())
                });
                if sent {
                    if hotkey.win {
                        mask_start_menu();
                    }
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// Modifier keys, left and right apart, as low-level hooks report them.
const MODIFIER_KEYS: [VIRTUAL_KEY; 8] = [
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LMENU,
    VK_RMENU,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LWIN,
    VK_RWIN,
];

/// After this long without a key event `HELD` is read from Windows again: a
/// held key repeats, so a pause means nothing is held, and a release the hook
/// didn't see (on the lock screen, in an elevated window) is forgotten.
const HELD_STALE_MS: u32 = 2000;

/// Bits of `MODIFIER_KEYS` held now, as Windows reports them.
fn modifiers_from_windows() -> u8 {
    MODIFIER_KEYS
        .iter()
        .enumerate()
        .filter(|(_, key)| unsafe { GetAsyncKeyState(key.0 as i32) } < 0)
        .fold(0, |held, (bit, _)| held | 1 << bit)
}

/// Follows the modifiers through the hook's own events and returns those
/// held now. `GetAsyncKeyState` lags here: with another app in front it
/// often still says Win is up when V arrives, which handed the first Win+V
/// to Windows.
fn track_modifiers(event: &KBDLLHOOKSTRUCT, key_down: bool) -> u8 {
    let (mut held, last) = HELD.get();
    if event.time.wrapping_sub(last) > HELD_STALE_MS {
        held = modifiers_from_windows();
    }
    // Some keyboards and injected input send the side-neutral codes.
    let vk = match VIRTUAL_KEY(event.vkCode as u16) {
        VK_CONTROL => VK_LCONTROL,
        VK_MENU => VK_LMENU,
        VK_SHIFT => VK_LSHIFT,
        vk => vk,
    };
    if let Some(bit) = MODIFIER_KEYS.iter().position(|&key| key == vk) {
        if key_down {
            held |= 1 << bit;
        } else {
            held &= !(1 << bit);
        }
    }
    HELD.set((held, event.time));
    held
}

/// Low-level hook while hotkeys are taken from Windows (see
/// `Registrar::update_hooked`): swallows the key of such a hotkey, so Windows
/// doesn't act on it, and reports the press. Every other key passes.
unsafe extern "system" fn hooked_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let key_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        // Injected modifiers (a paste's Ctrl) count: they change what's held.
        let held = if code == HC_ACTION as i32 {
            track_modifiers(event, key_down)
        } else {
            0
        };
        if code == HC_ACTION as i32 && event.flags & LLKHF_INJECTED != LLKHF_INJECTED {
            let vk = event.vkCode as u16;
            if SWALLOWED.get() == Some(vk) {
                // Auto-repeat while held is already handled, like MOD_NOREPEAT.
                if !key_down {
                    SWALLOWED.set(None);
                }
                return LRESULT(1);
            }
            if key_down && !is_modifier(vk) {
                let pressed_at = Instant::now();
                let any = |bits: u8| held & bits != 0;
                let hotkey = Hotkey {
                    ctrl: any(0b11),
                    alt: any(0b1100),
                    shift: any(0b11_0000),
                    win: any(0b1100_0000),
                    vk,
                };
                let target = HOOKED.with(|hooked| {
                    let hooked = hooked.borrow();
                    let found = hooked.iter().find(|(held, _)| *held == hotkey);
                    found.map(|(_, target)| target.clone())
                });
                if let Some(target) = target {
                    SWALLOWED.set(Some(vk));
                    if hotkey.win {
                        mask_start_menu();
                    }
                    HOOK_PRESSES.with(|senders| {
                        if let Some((presses, item_presses)) = senders.borrow().as_ref() {
                            match target {
                                Target::Launcher => drop(presses.try_send(pressed_at)),
                                Target::Item(item) => drop(item_presses.try_send(item)),
                            }
                        }
                    });
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// Taps a key no layout uses, so that releasing Win after a swallowed key
/// doesn't count as Win pressed alone, which opens the Start menu.
fn mask_start_menu() {
    const VK_NONE: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_NONE,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

fn is_modifier(vk: u16) -> bool {
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
    ]
    .iter()
    .any(|key| key.0 == vk)
}

const VK_SPACE: u16 = 0x20;

/// Named keys besides letters, digits and F1–F24. Punctuation uses the US
/// layout's virtual keys, which is what the `VK_OEM_*` codes are named after.
const NAMED_KEYS: &[(&str, u16)] = &[
    ("Space", VK_SPACE),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("Delete", 0x2E),
    ("Insert", 0x2D),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("`", 0xC0),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
    ("+", 0xBB),
];

fn vk_from_name(name: &str) -> Option<u16> {
    let name = name.trim();
    if let [ch] = name.as_bytes()
        && ch.is_ascii_alphanumeric()
    {
        return Some(ch.to_ascii_uppercase() as u16);
    }
    if let Some(n) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x70 + n - 1);
    }
    NAMED_KEYS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .map(|&(_, vk)| vk)
}

fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => NAMED_KEYS
            .iter()
            .find(|&&(_, known)| known == vk)
            .map_or_else(|| format!("0x{vk:02X}"), |(name, _)| (*name).to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_round_trip() {
        for text in [
            "Alt+Space",
            "Ctrl+Shift+K",
            "Win+F12",
            "Ctrl+Alt+/",
            "Shift+PageDown",
        ] {
            let hotkey = Hotkey::parse(text).unwrap_or_else(|| panic!("{text} should parse"));
            assert_eq!(hotkey.to_string(), text);
        }
        assert_eq!(Hotkey::parse("ctrl + space"), Hotkey::parse("Ctrl+Space"));
    }

    #[test]
    fn parses_the_plus_key() {
        let hotkey = Hotkey::parse("Ctrl++").unwrap();
        assert!(hotkey.ctrl);
        assert_eq!(hotkey.vk, 0xBB);
    }

    #[test]
    fn rejects_bare_keys_and_unknown_names() {
        assert_eq!(Hotkey::parse("Space"), None);
        assert_eq!(Hotkey::parse("Ctrl+NoSuchKey"), None);
        assert_eq!(Hotkey::parse("Hyper+Space"), None);
        assert_eq!(Hotkey::parse("Ctrl+F25"), None);
    }

    #[test]
    fn the_hook_takes_any_win_combination_but_win_l() {
        assert!(hookable(Hotkey::parse("Win+V").unwrap()).is_ok());
        assert!(hookable(Hotkey::parse("Win+Shift+K").unwrap()).is_ok());
        assert!(hookable(Hotkey::parse("Win+L").unwrap()).is_err());
        assert!(hookable(Hotkey::parse("Win+Shift+L").unwrap()).is_err());
    }

    #[test]
    fn defaults_display_as_before() {
        assert_eq!(Hotkey::DEFAULTS[0].to_string(), "Alt+Space");
        assert_eq!(Hotkey::DEFAULTS[1].to_string(), "Ctrl+Shift+Space");
    }
}
