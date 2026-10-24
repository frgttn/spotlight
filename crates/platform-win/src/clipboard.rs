//! The clipboard: watching it for the history, reading what was copied,
//! putting an entry back, and pasting into another app.
//!
//! The listener is a message-only window on a thread of its own, registered
//! with `AddClipboardFormatListener`, so it sees every change without polling
//! and without depending on the UI framework's message loop.

use std::{
    collections::HashMap,
    ffi::c_void,
    io::Cursor,
    mem::size_of,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use image::{ImageFormat, RgbaImage};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::{
            DataExchange::{
                AddClipboardFormatListener, CloseClipboard, EmptyClipboard, EnumClipboardFormats,
                GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT},
            Threading::{
                GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LEFT,
            },
            Shell::{DROPFILES, DragQueryFileW, HDROP},
            WindowsAndMessaging::{
                CreateWindowExW, DispatchMessageW, GetForegroundWindow, GetMessageW,
                GetWindowThreadProcessId, HWND_MESSAGE, IsWindow, MSG, PM_REMOVE, PeekMessageW,
                SetForegroundWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE,
            },
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, w},
};

/// What was copied.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    /// Paths of copied files and folders.
    Files(Vec<String>),
    Image(Image),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// One change of the clipboard the history should keep.
#[derive(Clone, Debug)]
pub struct Copied {
    pub content: Content,
    /// The app that copied it: its display name and program path. `None`
    /// when it can't be told, and when Spotlight itself wrote it.
    pub app: Option<(String, String)>,
}

/// Text longer than this (in UTF-16 units, ~4 MB) isn't kept.
const MAX_TEXT: usize = 2 * 1024 * 1024;
/// Neither are images larger than this many pixels (an 8K screenshot is 33 M).
const MAX_PIXELS: u64 = 50_000_000;
/// How long after a change the clipboard is read, for the app to finish
/// writing it.
const SETTLE: Duration = Duration::from_millis(100);

/// Set while Spotlight copies another app's selection to read it (see
/// `selection`): what lands on the clipboard then isn't the user's copy.
static BORROWED: AtomicBool = AtomicBool::new(false);
/// Changes up to this clipboard sequence number were Spotlight borrowing
/// the clipboard and putting it back; the history skips them.
static BORROWED_THROUGH: AtomicU32 = AtomicU32::new(0);

/// Starts watching the clipboard: everything copied from now on that isn't
/// marked private arrives on `copied`. The thread ends when `copied` closes.
pub fn listen(copied: async_channel::Sender<Copied>) -> Result<(), String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || unsafe {
            let window = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Spotlight clipboard listener"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            ) {
                Ok(window) => window,
                Err(err) => {
                    let _ = ready_tx.send(Err(format!("no listener window: {err}")));
                    return;
                }
            };
            if let Err(err) = AddClipboardFormatListener(window) {
                let _ = ready_tx.send(Err(format!("AddClipboardFormatListener: {err}")));
                return;
            }
            let _ = ready_tx.send(Ok(()));

            let mut apps = AppNames::default();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message != WM_CLIPBOARDUPDATE {
                    DispatchMessageW(&msg);
                    continue;
                }
                // Apps often write in several steps (text, then their own
                // formats), and opening the clipboard in between makes their
                // next step fail. Wait until they're done, and read once.
                thread::sleep(SETTLE);
                while PeekMessageW(
                    &mut msg,
                    Some(window),
                    WM_CLIPBOARDUPDATE,
                    WM_CLIPBOARDUPDATE,
                    PM_REMOVE,
                )
                .as_bool()
                {}
                if BORROWED.load(Ordering::Acquire)
                    || GetClipboardSequenceNumber() <= BORROWED_THROUGH.load(Ordering::Acquire)
                {
                    continue;
                }
                let Some(read) = read(window, &mut apps) else {
                    continue;
                };
                if copied.send_blocking(read).is_err() {
                    break;
                }
            }
        })
        .map_err(|err| err.to_string())?;
    ready_rx
        .recv()
        .map_err(|_| "the clipboard thread exited".to_owned())?
}

/// Formats an app sets on what it copies to keep it out of clipboard
/// history and monitors: password managers, private browser windows.
const PRIVATE_FORMATS: [PCWSTR; 2] = [
    w!("ExcludeClipboardContentFromMonitorProcessing"),
    w!("Clipboard Viewer Ignore"),
];

/// Reads the clipboard after a change, unless what's on it is private, empty
/// or of a kind the history doesn't keep. The clipboard stays open only
/// while the bytes are copied; images are converted after it's closed.
fn read(window: HWND, apps: &mut AppNames) -> Option<Copied> {
    let (raw, owner) = {
        let _open = Open::with_retries(window)?;
        if is_private() {
            return None;
        }
        let raw = read_files().or_else(read_text).or_else(read_image)?;
        (raw, unsafe { GetClipboardOwner() }.ok())
    };
    let content = match raw {
        Raw::Content(content) => content,
        Raw::Png(png) => {
            let (width, height) =
                image::ImageReader::with_format(Cursor::new(&png), ImageFormat::Png)
                    .into_dimensions()
                    .ok()?;
            if u64::from(width) * u64::from(height) > MAX_PIXELS {
                return None;
            }
            Content::Image(Image { png, width, height })
        }
        Raw::Dib(dib) => {
            let rgba = dib_to_rgba(&dib)?;
            let (width, height) = rgba.dimensions();
            let mut png = Vec::new();
            rgba.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
                .ok()?;
            Content::Image(Image { png, width, height })
        }
    };
    let app = owner
        .filter(|owner| !owner.is_invalid())
        .and_then(|owner| apps.of_window(owner));
    Some(Copied { content, app })
}

/// What was copied, as taken off the open clipboard.
enum Raw {
    Content(Content),
    Png(Vec<u8>),
    Dib(Vec<u8>),
}

/// Whether the app that copied asked to keep it out of clipboard history.
fn is_private() -> bool {
    unsafe {
        if PRIVATE_FORMATS
            .iter()
            .any(|&name| available(RegisterClipboardFormatW(name)))
        {
            return true;
        }
        // Windows' own history setting: a DWORD 0 means "don't keep this".
        let history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        available(history)
            && with_data(history, |bytes| bytes.get(..4) == Some(&[0; 4])) == Some(true)
    }
}

fn read_files() -> Option<Raw> {
    unsafe {
        if !available(CF_HDROP.0 as u32) {
            return None;
        }
        let handle = GetClipboardData(CF_HDROP.0 as u32).ok()?;
        let drop = HDROP(handle.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        let paths: Vec<String> = (0..count)
            .filter_map(|ix| {
                let len = DragQueryFileW(drop, ix, None) as usize;
                let mut buffer = vec![0u16; len + 1];
                let copied = DragQueryFileW(drop, ix, Some(&mut buffer)) as usize;
                (copied > 0).then(|| String::from_utf16_lossy(&buffer[..copied]))
            })
            .collect();
        (!paths.is_empty()).then_some(Raw::Content(Content::Files(paths)))
    }
}

fn read_text() -> Option<Raw> {
    let text = with_data(CF_UNICODETEXT.0 as u32, |bytes| {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .take(MAX_TEXT + 1)
            .collect();
        (units.len() <= MAX_TEXT).then(|| String::from_utf16_lossy(&units))
    })??;
    (!text.trim().is_empty()).then_some(Raw::Content(Content::Text(text)))
}

/// The copied image: the PNG an app put on the clipboard (browsers,
/// screenshot tools), otherwise its bitmap, to convert to PNG.
fn read_image() -> Option<Raw> {
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if available(png_format)
        && let Some(png) = with_data(png_format, <[u8]>::to_vec)
    {
        return Some(Raw::Png(png));
    }
    let format = [CF_DIBV5, CF_DIB]
        .into_iter()
        .map(|format| format.0 as u32)
        .find(|&format| available(format))?;
    with_data(format, <[u8]>::to_vec).map(Raw::Dib)
}

/// Decodes an uncompressed 24- or 32-bit DIB (`BITMAPINFOHEADER` or
/// `BITMAPV5HEADER`, then the pixels), the form apps copy bitmaps in.
fn dib_to_rgba(dib: &[u8]) -> Option<RgbaImage> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = u32_at(0)? as usize;
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32_at(16)?;
    let colors_used = u32_at(32)? as usize;
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    if width <= 0 || height == 0 || !matches!(bits, 24 | 32) {
        return None;
    }
    let (width, rows) = (width as u32, height.unsigned_abs());
    if u64::from(width) * u64::from(rows) > MAX_PIXELS {
        return None;
    }
    // Channel masks: in a V4/V5 header, or right after a plain one.
    let (masks, mut offset) = match compression {
        BI_RGB => ([0xff0000, 0xff00, 0xff, 0xff000000], header),
        BI_BITFIELDS if header >= 52 => {
            let alpha = if header >= 56 { u32_at(52)? } else { 0 };
            ([u32_at(40)?, u32_at(44)?, u32_at(48)?, alpha], header)
        }
        BI_BITFIELDS => (
            [u32_at(header)?, u32_at(header + 4)?, u32_at(header + 8)?, 0],
            header + 12,
        ),
        _ => return None,
    };
    offset += colors_used * 4;
    let stride = (width as usize * bits as usize).div_ceil(32) * 4;
    let pixels = dib.get(offset..offset + stride * rows as usize)?;

    let channel = |pixel: u32, mask: u32| -> u8 {
        if mask == 0 {
            return 0;
        }
        ((pixel & mask) >> mask.trailing_zeros()) as u8
    };
    let step = bits as usize / 8;
    let mut rgba = Vec::with_capacity(width as usize * rows as usize * 4);
    let mut any_alpha = false;
    for row in 0..rows as usize {
        // Positive heights are stored bottom row first.
        let source = if height > 0 {
            rows as usize - 1 - row
        } else {
            row
        };
        let line = &pixels[source * stride..][..width as usize * step];
        for px in line.chunks_exact(step) {
            let pixel = if step == 4 {
                u32::from_le_bytes([px[0], px[1], px[2], px[3]])
            } else {
                u32::from_le_bytes([px[0], px[1], px[2], 0])
            };
            let alpha = if bits == 32 {
                channel(pixel, masks[3])
            } else {
                255
            };
            any_alpha |= alpha != 0;
            rgba.extend([
                channel(pixel, masks[0]),
                channel(pixel, masks[1]),
                channel(pixel, masks[2]),
                alpha,
            ]);
        }
    }
    // Most 32-bit bitmaps leave the fourth byte at 0: they are opaque.
    if !any_alpha {
        rgba.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|px| px[3] = 255);
    }
    RgbaImage::from_raw(width, rows, rgba)
}

/// Puts `content` on the clipboard, as copying it in the first place did.
/// `owner` is a window of ours, which Windows records as the clipboard's owner.
pub fn write(owner: isize, content: &Content) -> Result<(), String> {
    let _open = Open::with_retries(HWND(owner as *mut c_void))
        .ok_or("the clipboard is busy in another app")?;
    unsafe {
        EmptyClipboard().map_err(|err| err.to_string())?;
        match content {
            Content::Text(text) => {
                let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
                set_data(CF_UNICODETEXT.0 as u32, as_bytes(&units))
            }
            Content::Files(paths) => {
                // DROPFILES, then the paths, each NUL-terminated, then one more NUL.
                let mut units: Vec<u16> = Vec::new();
                for path in paths {
                    units.extend(path.encode_utf16().chain([0]));
                }
                units.push(0);
                let header = DROPFILES {
                    pFiles: size_of::<DROPFILES>() as u32,
                    fWide: true.into(),
                    ..Default::default()
                };
                let mut data = std::slice::from_raw_parts(
                    &header as *const DROPFILES as *const u8,
                    size_of::<DROPFILES>(),
                )
                .to_vec();
                data.extend_from_slice(as_bytes(&units));
                set_data(CF_HDROP.0 as u32, &data)?;
                // Pasting in Explorer copies rather than moves.
                let effect = RegisterClipboardFormatW(w!("Preferred DropEffect"));
                set_data(effect, &1u32.to_le_bytes())
            }
            Content::Image(image) => {
                let rgba = image::load_from_memory_with_format(&image.png, ImageFormat::Png)
                    .map_err(|err| format!("unreadable image: {err}"))?
                    .into_rgba8();
                set_data(CF_DIB.0 as u32, &rgba_to_dib(&rgba))?;
                // Apps that understand PNG get transparency too.
                set_data(RegisterClipboardFormatW(w!("PNG")), &image.png)
            }
        }
    }
}

/// A bottom-up 32-bit `BITMAPINFOHEADER` DIB.
fn rgba_to_dib(rgba: &RgbaImage) -> Vec<u8> {
    let (width, height) = rgba.dimensions();
    let mut dib = Vec::with_capacity(40 + rgba.len());
    for field in [40u32, width, height] {
        dib.extend(field.to_le_bytes());
    }
    dib.extend(1u16.to_le_bytes()); // planes
    dib.extend(32u16.to_le_bytes()); // bits per pixel
    dib.extend([0u8; 24]); // BI_RGB, no size, resolution or palette
    for row in rgba.rows().rev() {
        for px in row {
            let [r, g, b, a] = px.0;
            dib.extend([b, g, r, a]);
        }
    }
    dib
}

fn as_bytes(units: &[u16]) -> &[u8] {
    // u16 has no padding and u8 has no alignment, so this view is sound.
    unsafe { std::slice::from_raw_parts(units.as_ptr().cast(), units.len() * 2) }
}

/// Copies `data` into global memory and hands it to the clipboard, which
/// then owns it.
unsafe fn set_data(format: u32, data: &[u8]) -> Result<(), String> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, data.len()).map_err(|err| err.to_string())?;
        let target = GlobalLock(memory) as *mut u8;
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        let _ = GlobalUnlock(memory);
        if let Err(err) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(err.to_string());
        }
        Ok(())
    }
}

fn available(format: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(format).is_ok() }
}

/// Runs `f` on the bytes of clipboard format `format`, while they're locked.
fn with_data<T>(format: u32, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        let data = GlobalLock(memory) as *const u8;
        if data.is_null() {
            return None;
        }
        let result = f(std::slice::from_raw_parts(data, size));
        let _ = GlobalUnlock(memory);
        Some(result)
    }
}

/// The clipboard, open; closed on drop.
struct Open;

impl Open {
    /// The app that just changed the clipboard may still hold it, so this
    /// tries for up to ~200 ms.
    fn with_retries(window: HWND) -> Option<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(Some(window)) }.is_ok() {
                return Some(Open);
            }
            thread::sleep(Duration::from_millis(20));
        }
        eprintln!("[clipboard] could not open the clipboard");
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Display names of programs ("Google Chrome"), by process, cached by path.
#[derive(Default)]
struct AppNames {
    names: HashMap<String, String>,
}

impl AppNames {
    /// The app owning `window`: its name and program path. `None` for
    /// Spotlight itself.
    fn of_window(&mut self, window: HWND) -> Option<(String, String)> {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
            return None;
        }
        let path = process_path(pid)?;
        let name = self
            .names
            .entry(path.clone())
            .or_insert_with(|| app_name(&path))
            .clone();
        Some((name, path))
    }
}

/// The program path of the app owning `window`.
pub(crate) fn window_program(window: isize) -> Option<String> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(HWND(window as *mut c_void), Some(&mut pid)) };
    (pid != 0).then(|| process_path(pid)).flatten()
}

pub(crate) fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

/// The program's description from its version resource ("Google Chrome"),
/// or its file name without `.exe`.
pub(crate) fn app_name(path: &str) -> String {
    let fallback = || {
        Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned())
    };
    file_description(path)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(fallback)
}

fn file_description(path: &str) -> Option<String> {
    unsafe {
        let path = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&path, None);
        if size == 0 {
            return None;
        }
        let mut block = vec![0u8; size as usize];
        GetFileVersionInfoW(&path, None, size, block.as_mut_ptr().cast()).ok()?;
        let block = block.as_ptr().cast();

        // The first language and code page the resource has strings for.
        let mut translation: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(
            block,
            w!("\\VarFileInfo\\Translation"),
            &mut translation,
            &mut len,
        )
        .as_bool()
            || len < 4
        {
            return None;
        }
        let language = *(translation as *const u16);
        let code_page = *(translation as *const u16).add(1);
        let key = HSTRING::from(format!(
            "\\StringFileInfo\\{language:04x}{code_page:04x}\\FileDescription"
        ));
        let mut value: *mut c_void = std::ptr::null_mut();
        if !VerQueryValueW(block, &key, &mut value, &mut len).as_bool() || len == 0 {
            return None;
        }
        let units = std::slice::from_raw_parts(value as *const u16, len as usize);
        let end = units
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]).trim().to_owned())
    }
}

/// The window that has the keyboard now, as a raw HWND (0 for none).
pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Activates `window` and presses Ctrl+V in it, to paste what was just put on
/// the clipboard, then Left `caret_back` times (to put the caret inside what
/// was pasted). Call it while Spotlight is still the foreground app: only
/// then may it hand the foreground to another window. Returns `false` (and
/// does nothing) when the window is gone.
pub fn paste_into(window: isize, caret_back: usize) -> bool {
    if !bring_to_front(window) {
        return false;
    }
    thread::spawn(move || {
        if !wait_for_focus(window) {
            eprintln!("[clipboard] the target window didn't come to the front; not pasting");
            return;
        }
        if !press_ctrl(VK_V) {
            // Windows blocks input into apps running as administrator.
            eprintln!("[clipboard] Ctrl+V was blocked");
            return;
        }
        if caret_back > 0 && !press(VK_LEFT, caret_back) {
            eprintln!("[clipboard] Left was blocked");
        }
    });
    true
}

/// Asks Windows to activate `window`. Only allowed while Spotlight is the
/// foreground app. `false` when the window is gone.
pub(crate) fn bring_to_front(window: isize) -> bool {
    let target = HWND(window as *mut c_void);
    unsafe {
        if window == 0 || !IsWindow(Some(target)).as_bool() {
            return false;
        }
        let _ = SetForegroundWindow(target);
    }
    true
}

/// Waits (up to half a second) until `window` is in front, then a moment
/// for it to put its caret and focus back. Keys go to whatever has focus.
pub(crate) fn wait_for_focus(window: isize) -> bool {
    let started = Instant::now();
    while unsafe { GetForegroundWindow() }.0 as isize != window {
        if started.elapsed() > Duration::from_millis(500) {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(30));
    true
}
const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(b'V' as u16);

/// Presses Ctrl and `key` in whatever window has the keyboard. `false` when
/// Windows blocked the keys.
pub(crate) fn press_ctrl(key: VIRTUAL_KEY) -> bool {
    let input = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    };
    let inputs = [
        input(VK_CONTROL, false),
        input(key, false),
        input(key, true),
        input(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// Presses `key` `times` times in whatever window has the keyboard. `false`
/// when Windows blocked the keys.
pub(crate) fn press(key: VIRTUAL_KEY, times: usize) -> bool {
    let input = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    };
    let inputs: Vec<INPUT> = (0..times)
        .flat_map(|_| [input(false), input(true)])
        .collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// Counts every clipboard change; it moves when an app copies.
pub(crate) fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Everything on the clipboard, format by format, to put back after
/// borrowing it.
pub(crate) struct Snapshot(Vec<(u32, Vec<u8>)>);

/// Formats held as GDI objects rather than memory; they can't be copied
/// byte for byte (Windows makes their memory forms, DIB and so on, itself).
const GDI_FORMATS: [u32; 6] = [2, 3, 9, 14, 0x82, 0x8E];
/// A clipboard larger than this isn't put back after borrowing it.
const MAX_SNAPSHOT: usize = 64 * 1024 * 1024;

impl Snapshot {
    /// Copies what's on the clipboard. `None` when it's busy or too large.
    pub(crate) fn take(owner: isize) -> Option<Self> {
        let _open = Open::with_retries(HWND(owner as *mut c_void))?;
        let mut formats = Vec::new();
        let mut total = 0;
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if GDI_FORMATS.contains(&format) {
                continue;
            }
            if let Some(bytes) = with_data(format, <[u8]>::to_vec) {
                total += bytes.len();
                if total > MAX_SNAPSHOT {
                    return None;
                }
                formats.push((format, bytes));
            }
        }
        Some(Snapshot(formats))
    }

    /// Puts the clipboard back as it was.
    pub(crate) fn restore(&self, owner: isize) -> Result<(), String> {
        let _open = Open::with_retries(HWND(owner as *mut c_void))
            .ok_or("the clipboard is busy in another app")?;
        unsafe {
            EmptyClipboard().map_err(|err| err.to_string())?;
            for (format, bytes) in &self.0 {
                set_data(*format, bytes)?;
            }
        }
        Ok(())
    }
}

/// Starts or ends borrowing the clipboard: the history ignores the changes
/// in between, and the one that puts it back.
pub(crate) fn set_borrowed(borrowed: bool) {
    if borrowed {
        BORROWED.store(true, Ordering::Release);
    } else {
        BORROWED_THROUGH.store(sequence(), Ordering::Release);
        BORROWED.store(false, Ordering::Release);
    }
}

/// The text on the clipboard now, if any.
pub(crate) fn text(owner: isize) -> Option<String> {
    let _open = Open::with_retries(HWND(owner as *mut c_void))?;
    match read_text()? {
        Raw::Content(Content::Text(text)) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_round_trips() {
        let mut rgba = RgbaImage::new(3, 2);
        for (x, y, px) in rgba.enumerate_pixels_mut() {
            *px = image::Rgba([x as u8 * 80, y as u8 * 120, 7, 255]);
        }
        assert_eq!(dib_to_rgba(&rgba_to_dib(&rgba)), Some(rgba));
    }

    #[test]
    fn opaque_bitmaps_with_an_empty_alpha_byte_stay_opaque() {
        let mut dib = rgba_to_dib(&RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])));
        for px in dib[40..].as_chunks_mut::<4>().0 {
            px[3] = 0;
        }
        let decoded = dib_to_rgba(&dib).unwrap();
        assert!(decoded.pixels().all(|px| px.0 == [1, 2, 3, 255]));
    }
}
