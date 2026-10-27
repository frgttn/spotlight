//! Window management: moving and resizing other apps' windows, switching to
//! them, closing them, and ending processes. Where layouts put a window is
//! worked out in `spotlight_core::window`; this only reads and moves them.
//!
//! Rectangles are the visible frame (DWM's extended frame bounds), not
//! `GetWindowRect`, which on Windows 10 and 11 includes an invisible resize
//! border of several pixels; halves would otherwise leave gaps.

use std::{
    collections::HashMap,
    ffi::c_void,
    mem::size_of,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

use spotlight_core::window::{self, Rect, WindowCommand};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, GetLastError, HWND, LPARAM, RECT,
            WPARAM,
        },
        Graphics::{
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
                MONITORINFO, MonitorFromWindow,
            },
        },
        Storage::Packaging::Appx::GetApplicationUserModelId,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_TERMINATE, TerminateProcess,
            },
        },
        UI::WindowsAndMessaging::{
            EnumChildWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
            GetWindowTextW, GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic,
            IsWindow, IsZoomed, PostMessageW, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
            SetWindowPos, ShowWindow, SwitchToThisWindow, WM_CLOSE, WS_EX_TOPMOST,
        },
    },
    core::{BOOL, PWSTR},
};

use crate::{
    clipboard::{app_name, process_path},
    system::Outcome,
    win,
};

/// A window as the switcher lists it.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    /// Raw HWND.
    pub hwnd: isize,
    pub title: String,
    /// The app's process: for Store apps, the app's own, not the frame host's.
    pub pid: u32,
    pub exe: Option<String>,
    /// "Google Chrome", from the program's version resource.
    pub app: String,
    pub minimized: bool,
}

/// A process of this user's session, as the process list shows it.
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    /// `chrome.exe`
    pub name: String,
    pub exe: Option<String>,
    /// Private memory, in bytes; 0 when it can't be read.
    pub memory: u64,
    /// How many app windows it has.
    pub windows: usize,
}

/// Where windows were before the first window command moved them, for Restore.
static SAVED: LazyLock<Mutex<HashMap<isize, Saved>>> = LazyLock::new(Default::default);
/// Pixels between windows placed by layouts (Settings).
static GAP: AtomicI32 = AtomicI32::new(0);
/// Whether running a half again cycles through thirds (Settings).
static CYCLE: AtomicBool = AtomicBool::new(true);

/// How layouts place windows from now on: `gap` pixels apart (and from the
/// screen's edges), and whether running a half again cycles through thirds.
pub fn set_layout_options(gap: i32, cycle: bool) {
    GAP.store(gap.clamp(0, window::MAX_GAP), Ordering::Relaxed);
    CYCLE.store(cycle, Ordering::Relaxed);
}

#[derive(Clone, Copy)]
struct Saved {
    frame: Rect,
    maximized: bool,
}

/// Shell windows that look like app windows but aren't anything to arrange.
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];
/// Store apps' windows belong to this process; the app is a child window's.
const FRAME_HOST: &str = "applicationframehost.exe";

/// A running app, as root search matches it to an installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningApp {
    /// The process to quit: the one with windows, if any.
    pub pid: u32,
    pub exe: Option<String>,
    /// A packaged app's AUMID (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`).
    pub aumid: Option<String>,
}

/// The apps running in this session that have windows, one per program
/// (or packaged app): its process with windows.
pub fn running_apps() -> Vec<RunningApp> {
    let mut apps: Vec<RunningApp> = Vec::new();
    for raw in win::app_windows() {
        let pid = app_pid(raw);
        if pid == 0 || apps.iter().any(|app| app.pid == pid) {
            continue;
        }
        let app = RunningApp {
            pid,
            exe: process_path(pid),
            aumid: process_aumid(pid),
        };
        let same = |known: &RunningApp| match (&app.aumid, &known.aumid) {
            (Some(aumid), Some(other)) => aumid == other,
            (None, None) => app.exe.is_some() && known.exe == app.exe,
            _ => false,
        };
        if !apps.iter().any(same) {
            apps.push(app);
        }
    }
    apps
}

/// The AUMID of a packaged app's process; `None` for other programs.
fn process_aumid(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 256];
        let mut len = buffer.len() as u32;
        let result = GetApplicationUserModelId(process, &mut len, Some(PWSTR(buffer.as_mut_ptr())));
        let _ = CloseHandle(process);
        if result != ERROR_SUCCESS || len == 0 {
            return None;
        }
        let units = &buffer[..(len as usize).saturating_sub(1)];
        Some(String::from_utf16_lossy(units))
    }
}

/// Every app window except Spotlight's, front to back.
pub fn windows() -> Vec<WindowInfo> {
    let own = unsafe { GetCurrentProcessId() };
    let mut names: HashMap<String, String> = HashMap::new();
    win::app_windows()
        .into_iter()
        .filter_map(|raw| {
            let pid = app_pid(raw);
            if pid == own || pid == 0 {
                return None;
            }
            let exe = process_path(pid);
            let app = match &exe {
                Some(path) => names
                    .entry(path.clone())
                    .or_insert_with(|| app_name(path))
                    .clone(),
                None => String::new(),
            };
            Some(WindowInfo {
                hwnd: raw,
                title: window_text(raw),
                pid,
                exe,
                app,
                minimized: unsafe { IsIconic(hwnd(raw)).as_bool() },
            })
        })
        .collect()
}

/// This session's processes (not services, not Spotlight), most memory first.
pub fn processes() -> Vec<ProcessInfo> {
    let own = unsafe { GetCurrentProcessId() };
    let session = session_of(own);
    let mut window_counts: HashMap<u32, usize> = HashMap::new();
    for raw in win::app_windows() {
        *window_counts.entry(app_pid(raw)).or_default() += 1;
    }
    let mut processes = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return processes;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let pid = entry.th32ProcessID;
            if pid != own && pid > 4 && session_of(pid) == session {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                processes.push(ProcessInfo {
                    pid,
                    name: String::from_utf16_lossy(&entry.szExeFile[..len]),
                    exe: process_path(pid),
                    memory: private_memory(pid),
                    windows: window_counts.get(&pid).copied().unwrap_or(0),
                });
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    processes.sort_by_key(|process| std::cmp::Reverse(process.memory));
    processes
}

/// Does `command` to window `target` (a raw HWND).
pub fn run(target: isize, command: WindowCommand) -> Outcome {
    let window = hwnd(target);
    if target == 0 || !unsafe { IsWindow(Some(window)).as_bool() } {
        return Err("That window is gone".into());
    }
    if SHELL_CLASSES.contains(&class_name(target).as_str()) {
        return Err("No app window is active".into());
    }
    if command.moves() {
        let maximized = unsafe { IsZoomed(window).as_bool() };
        // A maximized window placed somewhere else starts from where it was
        // before it was maximized, which Windows keeps.
        if maximized && command.places() {
            unsafe {
                let _ = ShowWindow(window, SW_RESTORE);
            }
        }
        remember(target, maximized);
    }
    match command {
        WindowCommand::Focus => focus(target),
        WindowCommand::Close => {
            unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                .map_err(|err| format!("Couldn't close the window: {err}"))?;
            Ok(None)
        }
        WindowCommand::Minimize => {
            unsafe {
                let _ = ShowWindow(window, SW_MINIMIZE);
            }
            Ok(None)
        }
        WindowCommand::MinimizeDisplay => minimize_display(target),
        WindowCommand::Maximize => {
            unsafe {
                let _ = ShowWindow(window, SW_MAXIMIZE);
            }
            Ok(None)
        }
        WindowCommand::ToggleAlwaysOnTop => toggle_topmost(target),
        WindowCommand::FocusNextAppWindow => cycle(target, 1),
        WindowCommand::FocusPreviousAppWindow => cycle(target, -1),
        WindowCommand::Restore => restore(target),
        WindowCommand::NextDisplay | WindowCommand::PreviousDisplay => {
            let step = if command == WindowCommand::NextDisplay {
                1
            } else {
                -1
            };
            let work = work_area(target);
            let Some(to) = window::neighbour_display(&displays(), work, step) else {
                return Ok(Some("There is only one display".into()));
            };
            let maximized = unsafe { IsZoomed(window).as_bool() };
            set_frame(target, window::move_between(frame(target), work, to))?;
            if maximized {
                unsafe {
                    let _ = ShowWindow(window, SW_MAXIMIZE);
                }
            }
            Ok(None)
        }
        command => {
            let (current, work) = (frame(target), work_area(target));
            let gap = GAP.load(Ordering::Relaxed);
            let command = match command {
                WindowCommand::Area(part) if CYCLE.load(Ordering::Relaxed) => {
                    WindowCommand::Area(window::cycled(part, current, work, gap))
                }
                command => command,
            };
            match window::place(command, current, work, gap) {
                Some(rect) => set_frame(target, rect).map(|()| None),
                None => Ok(None),
            }
        }
    }
}

/// Moves and resizes `target` to the numbers given (see
/// [`window::frame_from`]); Restore puts it back.
pub fn set_frame_from(target: isize, numbers: [&String; 4]) -> Outcome {
    let window = hwnd(target);
    if target == 0 || !unsafe { IsWindow(Some(window)).as_bool() } {
        return Err("That window is gone".into());
    }
    let maximized = unsafe { IsZoomed(window).as_bool() };
    if maximized {
        unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
        }
    }
    remember(target, maximized);
    let rect = window::frame_from(
        frame(target),
        work_area(target),
        numbers.map(String::as_str),
    )?;
    set_frame(target, rect).map(|()| None)
}

/// Where `target` is now: its frame relative to its display's work area.
pub fn position(target: isize) -> Option<Rect> {
    if target == 0 || !unsafe { IsWindow(Some(hwnd(target))).as_bool() } {
        return None;
    }
    let (frame, work) = (frame(target), work_area(target));
    Some(Rect {
        x: frame.x - work.x,
        y: frame.y - work.y,
        ..frame
    })
}

/// Asks every window of process `pid` to close.
pub fn quit_app(pid: u32) -> Outcome {
    let windows: Vec<isize> = win::app_windows()
        .into_iter()
        .filter(|&raw| app_pid(raw) == pid || thread_pid(raw) == pid)
        .collect();
    if windows.is_empty() {
        return Err("It has no windows to close; Force Quit ends it".into());
    }
    for raw in windows {
        unsafe {
            let _ = PostMessageW(Some(hwnd(raw)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    Ok(None)
}

/// Ends process `pid` at once.
pub fn force_quit(pid: u32) -> Outcome {
    let name = process_path(pid)
        .map(|path| app_name(&path))
        .unwrap_or_else(|| format!("process {pid}"));
    if pid <= 4 || pid == unsafe { GetCurrentProcessId() } {
        return Err(format!("Won't end {name}"));
    }
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, false, pid).map_err(|err| {
            if err.code() == ERROR_ACCESS_DENIED.to_hresult() {
                format!("Windows doesn't let Spotlight end {name} (it runs as administrator)")
            } else {
                format!("Couldn't end {name}: {err}")
            }
        })?;
        let ended = TerminateProcess(process, 1);
        let _ = CloseHandle(process);
        ended.map_err(|err| format!("Couldn't end {name}: {err}"))?;
    }
    Ok(Some(format!("Ended {name}")))
}

/// Brings `target` to the front, restoring it if minimized. Works while
/// Spotlight got the last input (the key that ran the command).
fn focus(target: isize) -> Outcome {
    let window = hwnd(target);
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        if !SetForegroundWindow(window).as_bool() {
            // Also allowed to bring it up when SetForegroundWindow isn't.
            SwitchToThisWindow(window, true);
        }
    }
    Ok(None)
}

/// Minimizes every app window on `target`'s display, `target` too.
fn minimize_display(target: isize) -> Outcome {
    let display = unsafe { MonitorFromWindow(hwnd(target), MONITOR_DEFAULTTONEAREST) };
    let own = unsafe { GetCurrentProcessId() };
    let mut minimized = 0;
    for raw in win::app_windows() {
        let window = hwnd(raw);
        unsafe {
            if thread_pid(raw) == own
                || IsIconic(window).as_bool()
                || MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) != display
            {
                continue;
            }
            let _ = ShowWindow(window, SW_MINIMIZE);
        }
        minimized += 1;
    }
    Ok((minimized == 0).then(|| "No windows on this display".into()))
}

fn toggle_topmost(target: isize) -> Outcome {
    let window = hwnd(target);
    let topmost = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
    let after = if topmost {
        HWND_NOTOPMOST
    } else {
        HWND_TOPMOST
    };
    unsafe {
        SetWindowPos(
            window,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
    .map_err(|err| denied_or(err, "Couldn't change the window"))?;
    let title = short_title(target);
    Ok(Some(if topmost {
        format!("{title}: Always on Top Off")
    } else {
        format!("{title}: Always on Top On")
    }))
}

/// Focuses the app's next (`step` 1) or previous (-1) window, in a fixed
/// order so pressing it again goes on to the one after.
fn cycle(target: isize, step: i32) -> Outcome {
    let exe = process_path(app_pid(target));
    let mut siblings: Vec<isize> = win::app_windows()
        .into_iter()
        .filter(|&raw| raw == target || (exe.is_some() && process_path(app_pid(raw)) == exe))
        .collect();
    if siblings.len() < 2 {
        return Ok(Some("The app has no other windows".into()));
    }
    siblings.sort_unstable();
    let ix = siblings.iter().position(|&raw| raw == target).unwrap_or(0);
    let next = (ix as i32 + step).rem_euclid(siblings.len() as i32) as usize;
    focus(siblings[next])
}

/// Keeps where `target` is (unless it's already kept): its frame now, and
/// whether it was `maximized` before the command (it may have just been
/// restored to its own place).
fn remember(target: isize, maximized: bool) {
    let mut saved = SAVED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Forget windows that are gone, so handles reused later don't match.
    saved.retain(|&raw, _| unsafe { IsWindow(Some(hwnd(raw))).as_bool() });
    saved.entry(target).or_insert_with(|| Saved {
        frame: frame(target),
        maximized,
    });
}

fn restore(target: isize) -> Outcome {
    let saved = SAVED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&target);
    let Some(saved) = saved else {
        return Ok(Some("Nothing to restore".into()));
    };
    set_frame(target, saved.frame)?;
    if saved.maximized {
        unsafe {
            let _ = ShowWindow(hwnd(target), SW_MAXIMIZE);
        }
    }
    Ok(None)
}

/// Moves and resizes `target` so its visible frame is `rect`.
fn set_frame(target: isize, rect: Rect) -> Result<(), String> {
    let window = hwnd(target);
    unsafe {
        if IsZoomed(window).as_bool() || IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
    }
    // A move to a display with another scale makes the app resize itself
    // once it gets there; the second pass corrects for that.
    for _ in 0..2 {
        let (outer, visible) = (outer_rect(target), frame(target));
        let (left, top) = (visible.x - outer.x, visible.y - outer.y);
        let (right, bottom) = (
            outer.right() - visible.right(),
            outer.bottom() - visible.bottom(),
        );
        unsafe {
            SetWindowPos(
                window,
                None,
                rect.x - left,
                rect.y - top,
                rect.width + left + right,
                rect.height + top + bottom,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        }
        .map_err(|err| denied_or(err, "Couldn't move the window"))?;
        if frame(target) == rect {
            break;
        }
    }
    Ok(())
}

fn denied_or(err: windows::core::Error, message: &str) -> String {
    if err.code() == ERROR_ACCESS_DENIED.to_hresult()
        || unsafe { GetLastError() } == ERROR_ACCESS_DENIED
    {
        "Windows doesn't let Spotlight move windows of apps running as administrator".into()
    } else {
        format!("{message}: {err}")
    }
}

/// The visible frame.
fn frame(target: isize) -> Rect {
    let mut rect = RECT::default();
    let read = unsafe {
        DwmGetWindowAttribute(
            hwnd(target),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
    };
    match read {
        Ok(()) => from_rect(rect),
        Err(_) => outer_rect(target),
    }
}

fn outer_rect(target: isize) -> Rect {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd(target), &mut rect);
    }
    from_rect(rect)
}

/// The work area (the screen without the taskbar) of the display the
/// window is mostly on.
fn work_area(target: isize) -> Rect {
    let monitor = unsafe { MonitorFromWindow(hwnd(target), MONITOR_DEFAULTTONEAREST) };
    monitor_work(monitor).unwrap_or_default()
}

fn monitor_work(monitor: HMONITOR) -> Option<Rect> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then(|| from_rect(info.rcWork))
}

/// Every display's work area.
fn displays() -> Vec<Rect> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        displays: LPARAM,
    ) -> BOOL {
        if let Some(work) = monitor_work(monitor) {
            unsafe { (*(displays.0 as *mut Vec<Rect>)).push(work) };
        }
        true.into()
    }
    let mut displays: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut displays as *mut Vec<Rect> as isize),
        );
    }
    displays
}

/// The process the window belongs to; for a Store app's frame, the app's.
fn app_pid(raw: isize) -> u32 {
    let pid = thread_pid(raw);
    let is_frame_host =
        process_path(pid).is_some_and(|path| path.to_ascii_lowercase().ends_with(FRAME_HOST));
    if !is_frame_host {
        return pid;
    }
    unsafe extern "system" fn other_process(child: HWND, found: LPARAM) -> BOOL {
        let (host, app) = unsafe { &mut *(found.0 as *mut (u32, u32)) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(child, Some(&mut pid)) };
        if pid != *host {
            *app = pid;
            return false.into();
        }
        true.into()
    }
    let mut found = (pid, pid);
    unsafe {
        let _ = EnumChildWindows(
            Some(hwnd(raw)),
            Some(other_process),
            LPARAM(&mut found as *mut (u32, u32) as isize),
        );
    }
    found.1
}

fn thread_pid(raw: isize) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd(raw), Some(&mut pid)) };
    pid
}

fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(pid, &mut session) }
        .is_ok()
        .then_some(session)
}

fn private_memory(pid: u32) -> u64 {
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return 0;
        };
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let read = GetProcessMemoryInfo(
            process,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        );
        let _ = CloseHandle(process);
        if read.is_ok() {
            counters.PrivateUsage as u64
        } else {
            0
        }
    }
}

fn window_text(raw: isize) -> String {
    let mut buffer = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd(raw), &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

/// The title cut to a length that fits the HUD.
fn short_title(raw: isize) -> String {
    let title = window_text(raw);
    if title.chars().count() > 40 {
        format!("{}…", title.chars().take(39).collect::<String>())
    } else {
        title
    }
}

fn class_name(raw: isize) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd(raw), &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

fn from_rect(rect: RECT) -> Rect {
    Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    )
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_windows_and_processes() {
        // Whatever is open; only checks that reading works and finds something.
        let processes = processes();
        assert!(
            processes
                .iter()
                .any(|process| process.name.eq_ignore_ascii_case("explorer.exe"))
        );
        assert!(processes.iter().any(|process| process.memory > 0));
        for window in windows() {
            assert_ne!(window.pid, 0);
        }
        assert!(!displays().is_empty());
    }

    /// Arranges a window of its own (shown without taking focus, then
    /// destroyed). Run by hand: `cargo test -p spotlight-platform-win -- --ignored`.
    #[test]
    #[ignore = "shows a window"]
    fn arranges_a_window() {
        use windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE,
                WS_OVERLAPPEDWINDOW,
            },
            core::w,
        };

        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Spotlight window management test"),
                WS_OVERLAPPEDWINDOW,
                200,
                150,
                640,
                420,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        unsafe {
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        let raw = window.0 as isize;
        let start = frame(raw);
        let work = work_area(raw);
        let left = window::LAYOUTS
            .iter()
            .find(|(id, ..)| *id == "left-half")
            .unwrap()
            .3;

        run(raw, left).unwrap();
        assert_eq!(frame(raw), window::place(left, start, work, 0).unwrap());
        run(raw, WindowCommand::Center).unwrap();
        let centered = frame(raw);
        assert_eq!(centered.x - work.x, work.right() - centered.right());
        run(raw, WindowCommand::Restore).unwrap();
        assert_eq!(frame(raw), start);
        assert_eq!(
            run(raw, WindowCommand::Restore).unwrap().as_deref(),
            Some("Nothing to restore")
        );

        run(raw, WindowCommand::Maximize).unwrap();
        assert!(unsafe { IsZoomed(window).as_bool() });
        run(raw, WindowCommand::Restore).unwrap();
        assert!(!unsafe { IsZoomed(window).as_bool() });
        assert_eq!(frame(raw), start);

        // After Maximize, Center starts from the window's own size.
        run(raw, WindowCommand::Maximize).unwrap();
        run(raw, WindowCommand::Center).unwrap();
        let centered = frame(raw);
        assert!(!unsafe { IsZoomed(window).as_bool() });
        assert_eq!(
            (centered.width, centered.height),
            (start.width, start.height)
        );
        run(raw, WindowCommand::Restore).unwrap();
        assert_eq!(frame(raw), start);

        let on = run(raw, WindowCommand::ToggleAlwaysOnTop).unwrap().unwrap();
        assert!(on.ends_with("Always on Top On"), "{on}");
        let off = run(raw, WindowCommand::ToggleAlwaysOnTop).unwrap().unwrap();
        assert!(off.ends_with("Always on Top Off"), "{off}");

        unsafe { DestroyWindow(window).unwrap() };
        assert!(run(raw, WindowCommand::Center).is_err());
    }
}
