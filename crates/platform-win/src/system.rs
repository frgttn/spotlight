//! System commands: power, session, Recycle Bin, theme, sound, Bluetooth,
//! HDR, drives.
//!
//! Each command runs on a short-lived thread of its own: several block (the
//! Recycle Bin shows its own confirmation, Bluetooth waits on WinRT) and they
//! need COM in different apartments.

use std::{ffi::c_void, mem::size_of, os::windows::process::CommandExt, process, ptr, thread};

use spotlight_core::item::SystemCommand;
use windows::{
    Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState},
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
            DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_TYPE,
            DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
            DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
            DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
            DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
            QueryDisplayConfig,
        },
        Foundation::{ERROR_SUCCESS, LPARAM, LUID, POINT, WPARAM},
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW,
            MonitorFromPoint,
        },
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole,
            eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance,
                CoInitializeEx,
            },
            Power::SetSuspendState,
            Registry::{
                HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
            },
            Shutdown::{
                EWX_LOGOFF, ExitWindowsEx, LockWorkStation, SHTDN_REASON_FLAG_PLANNED,
                SHTDN_REASON_MAJOR_OTHER,
            },
            Threading::CREATE_NO_WINDOW,
        },
        UI::{
            Shell::SHEmptyRecycleBinW,
            WindowsAndMessaging::{
                GetCursorPos, HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
                WM_SETTINGCHANGE,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::drives;

/// What a command reports: a short message for the HUD ("Bluetooth Off",
/// "Volume 40%"), nothing, or why it failed, in words for the user.
pub type Outcome = Result<Option<String>, String>;

/// Runs `command` in the background; its outcome arrives on the returned channel.
pub fn spawn(command: SystemCommand) -> async_channel::Receiver<Outcome> {
    let (tx, rx) = async_channel::bounded(1);
    thread::spawn(move || {
        let outcome = run_blocking(command);
        if let Err(err) = &outcome {
            eprintln!("[system] {command:?} failed: {err}");
        }
        let _ = tx.send_blocking(outcome);
    });
    rx
}

pub(crate) fn run_blocking(command: SystemCommand) -> Outcome {
    let done = |result: windows::core::Result<()>| result.map(|()| None).map_err(message);
    unsafe {
        match command {
            SystemCommand::Lock => done(LockWorkStation()),
            SystemCommand::Sleep => {
                // Sleeps, or hibernates if the power settings turned sleep into hibernate.
                if SetSuspendState(false, false, false) {
                    Ok(None)
                } else {
                    Err(message(windows::core::Error::from_win32()))
                }
            }
            // `shutdown.exe` takes care of the shutdown privilege, which
            // `ExitWindowsEx` would make us enable first.
            SystemCommand::Restart => shutdown(&["/r", "/t", "0"]),
            SystemCommand::ShutDown => shutdown(&["/s", "/t", "0"]),
            SystemCommand::SignOut => done(ExitWindowsEx(
                EWX_LOGOFF,
                SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED,
            )),
            // No flags: Windows asks for confirmation and shows progress itself.
            // Declining, or a bin that's already empty, also lands in `Err`;
            // either way Windows has already said what happened.
            SystemCommand::EmptyRecycleBin => {
                let _ = SHEmptyRecycleBinW(None, PCWSTR::null(), 0);
                Ok(None)
            }
            SystemCommand::ToggleDarkMode => toggle_dark_mode(),
            SystemCommand::ToggleMute => toggle_mute(),
            // Steps of 10%, from the nearest step, as the volume keys feel.
            SystemCommand::VolumeUp => set_volume(|now| ((now * 10.).round() + 1.) / 10.),
            SystemCommand::VolumeDown => set_volume(|now| ((now * 10.).round() - 1.) / 10.),
            SystemCommand::SetVolume(percent) => set_volume(|_| f32::from(percent) / 100.),
            SystemCommand::ToggleBluetooth => toggle_bluetooth(),
            SystemCommand::ToggleHdr => toggle_hdr(),
            SystemCommand::Eject(letter) => drives::eject(letter).map(Some),
            SystemCommand::EjectAll => drives::eject_all().map(Some),
        }
    }
}

fn message(err: windows::core::Error) -> String {
    let text = err.message();
    if text.is_empty() {
        format!("Error {:#x}", err.code().0)
    } else {
        text
    }
}

fn shutdown(args: &[&str]) -> Outcome {
    process::Command::new("shutdown.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map(|_| None)
        .map_err(|err| err.to_string())
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

/// Flips apps and the system (taskbar, Start) between light and dark together,
/// following the apps setting, as the Settings app's single switch does.
unsafe fn toggle_dark_mode() -> Outcome {
    unsafe {
        let to_dark = !dark_mode();
        // The registry values say "light": 1 is light, 0 is dark.
        let next = u32::from(!to_dark);
        for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
            let status = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                name,
                REG_DWORD.0,
                Some(&next as *const u32 as *const c_void),
                size_of::<u32>() as u32,
            );
            if status != ERROR_SUCCESS {
                return Err(message(status.into()));
            }
        }
        // What the Settings app broadcasts, so open windows repaint in the new theme.
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            200,
            None,
        );
        Ok(Some(
            if to_dark { "Dark Mode" } else { "Light Mode" }.into(),
        ))
    }
}

/// Whether apps currently use the dark theme.
pub fn dark_mode() -> bool {
    // A missing value means light, Windows' default.
    let mut light = 1u32;
    let mut size = size_of::<u32>() as u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut u32 as *mut c_void),
            Some(&mut size),
        );
    }
    light == 0
}

/// The default playback device's volume control.
unsafe fn speakers() -> Result<IAudioEndpointVolume, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(message)?;
        let device = devices
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|_| "No playback device".to_owned())?;
        device.Activate(CLSCTX_ALL, None).map_err(message)
    }
}

unsafe fn toggle_mute() -> Outcome {
    unsafe {
        let volume = speakers()?;
        let muted = volume.GetMute().map_err(message)?.as_bool();
        volume.SetMute(!muted, ptr::null()).map_err(message)?;
        Ok(Some(if muted { "Unmuted" } else { "Muted" }.into()))
    }
}

/// Sets the volume to `next(current)` (both 0–1). Turning it up also unmutes,
/// like the volume keys do.
unsafe fn set_volume(next: impl FnOnce(f32) -> f32) -> Outcome {
    unsafe {
        let volume = speakers()?;
        let now = volume.GetMasterVolumeLevelScalar().map_err(message)?;
        let level = next(now).clamp(0., 1.);
        volume
            .SetMasterVolumeLevelScalar(level, ptr::null())
            .map_err(message)?;
        if level > now && volume.GetMute().map_err(message)?.as_bool() {
            volume.SetMute(false, ptr::null()).map_err(message)?;
        }
        Ok(Some(format!("Volume {}%", (level * 100.).round())))
    }
}

/// Turns the (first) Bluetooth radio on or off, as the Quick Settings tile does.
fn toggle_bluetooth() -> Outcome {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let denied = "Windows doesn't let Spotlight change Bluetooth (Settings › Privacy › Radios)";
    let access = Radio::RequestAccessAsync()
        .and_then(|operation| operation.get())
        .map_err(message)?;
    if access != RadioAccessStatus::Allowed {
        return Err(denied.into());
    }
    let radios = Radio::GetRadiosAsync()
        .and_then(|operation| operation.get())
        .map_err(message)?;
    for ix in 0..radios.Size().map_err(message)? {
        let radio = radios.GetAt(ix).map_err(message)?;
        if radio.Kind().map_err(message)? != RadioKind::Bluetooth {
            continue;
        }
        let on = radio.State().map_err(message)? == RadioState::On;
        let target = if on { RadioState::Off } else { RadioState::On };
        let result = radio
            .SetStateAsync(target)
            .and_then(|operation| operation.get())
            .map_err(message)?;
        if result != RadioAccessStatus::Allowed {
            return Err(denied.into());
        }
        return Ok(Some(
            if on { "Bluetooth Off" } else { "Bluetooth On" }.into(),
        ));
    }
    Err("No Bluetooth adapter found".into())
}

/// Turns HDR on or off on the display under the mouse pointer, which is also
/// where the launcher opens. Other displays keep their setting.
fn toggle_hdr() -> Outcome {
    let displays = hdr_displays()?;
    if displays.is_empty() {
        return Err("No display here supports HDR".into());
    }
    let pointer = display_under_pointer();
    let display = match displays
        .iter()
        .find(|display| Some(&display.gdi_name) == pointer.as_ref())
    {
        Some(display) => display,
        None if displays.len() == 1 => &displays[0],
        None => return Err("This display doesn't support HDR".into()),
    };
    let enable = !display.enabled;
    let status = if display.windows_11_api {
        let mut state = SetHdrState::default();
        state.header.r#type = DEVICE_INFO_SET_HDR_STATE;
        state.header.size = size_of::<SetHdrState>() as u32;
        state.header.adapterId = display.adapter;
        state.header.id = display.target;
        // Bit 0: enableHdr.
        state.value = u32::from(enable);
        unsafe { DisplayConfigSetDeviceInfo(&state.header) }
    } else {
        let mut state = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE::default();
        state.header.r#type = DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE;
        state.header.size = size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32;
        state.header.adapterId = display.adapter;
        state.header.id = display.target;
        // Bit 0: enableAdvancedColor.
        state.Anonymous.value = u32::from(enable);
        unsafe { DisplayConfigSetDeviceInfo(&state.header) }
    };
    if status != 0 {
        return Err(format!("Couldn't change HDR (error {status})"));
    }
    Ok(Some(if enable { "HDR On" } else { "HDR Off" }.into()))
}

struct HdrDisplay {
    adapter: LUID,
    target: u32,
    /// `\\.\DISPLAY1`: matches `MONITORINFOEXW::szDevice`.
    gdi_name: String,
    enabled: bool,
    /// Read (and so switched) through the Windows 11 HDR calls below.
    windows_11_api: bool,
}

// Windows 11 split HDR out of "advanced color": the older call's
// advancedColorEnabled stays set in SDR whenever wide color / auto color
// management is on, so it can't tell whether HDR is on. These come from the
// SDK's wingdi.h (10.0.26100); the `windows` crate doesn't have them yet.
const DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const DEVICE_INFO_SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(16);
/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2`.
#[repr(C)]
#[derive(Default)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// Bit 4: highDynamicRangeSupported, bit 5: highDynamicRangeUserEnabled.
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}
/// `DISPLAYCONFIG_SET_HDR_STATE`.
#[repr(C)]
#[derive(Default)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

/// Whether a display supports HDR and has it on, and whether that came from
/// the Windows 11 call (Windows 10 only has the older one).
unsafe fn hdr_state(adapter: LUID, target: u32) -> Option<(bool, bool, bool)> {
    unsafe {
        let mut info = AdvancedColorInfo2::default();
        info.header.r#type = DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2;
        info.header.size = size_of::<AdvancedColorInfo2>() as u32;
        info.header.adapterId = adapter;
        info.header.id = target;
        if DisplayConfigGetDeviceInfo(&mut info.header) == 0 {
            return Some((info.value & 1 << 4 != 0, info.value & 1 << 5 != 0, true));
        }

        let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
        info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
        info.header.size = size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
        info.header.adapterId = adapter;
        info.header.id = target;
        if DisplayConfigGetDeviceInfo(&mut info.header) != 0 {
            return None;
        }
        // Bit 0: advancedColorSupported, bit 1: advancedColorEnabled.
        let flags = info.Anonymous.value;
        Some((flags & 1 != 0, flags & 2 != 0, false))
    }
}

/// The GDI device name of the monitor under the mouse pointer.
fn display_under_pointer() -> Option<String> {
    unsafe {
        let mut cursor = POINT::default();
        GetCursorPos(&mut cursor).ok()?;
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut MONITORINFO)
            .as_bool()
            .then(|| wide_to_string(&info.szDevice))
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&ch| ch == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Active displays that support HDR ("advanced color"), and whether it is on.
fn hdr_displays() -> Result<Vec<HdrDisplay>, String> {
    unsafe {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        let status =
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count);
        if status != ERROR_SUCCESS {
            return Err(message(status.into()));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(message(status.into()));
        }
        paths.truncate(path_count as usize);

        let mut displays = Vec::new();
        for path in &paths {
            let Some((true, enabled, windows_11_api)) =
                hdr_state(path.targetInfo.adapterId, path.targetInfo.id)
            else {
                continue;
            };
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            let gdi_name = if DisplayConfigGetDeviceInfo(&mut source.header) == 0 {
                wide_to_string(&source.viewGdiDeviceName)
            } else {
                String::new()
            };
            displays.push(HdrDisplay {
                adapter: path.targetInfo.adapterId,
                target: path.targetInfo.id,
                gdi_name,
                enabled,
                windows_11_api,
            });
        }
        Ok(displays)
    }
}

/// For each active display that supports HDR: its GDI name, whether HDR is
/// on, and whether the Windows 11 call answered. For diagnostics.
pub fn hdr_status() -> Result<Vec<(String, bool, bool)>, String> {
    Ok(hdr_displays()?
        .into_iter()
        .map(|display| (display.gdi_name, display.enabled, display.windows_11_api))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sets the volume to the level it already has, through the real call.
    /// Run by hand: `cargo test -p spotlight-platform-win -- --ignored`.
    #[test]
    #[ignore = "talks to the audio device"]
    fn sets_volume_to_its_current_level() {
        let now = unsafe { speakers().unwrap().GetMasterVolumeLevelScalar().unwrap() };
        let percent = (now * 100.).round() as u8;
        let outcome = run_blocking(SystemCommand::SetVolume(percent));
        eprintln!("volume {percent}% -> {outcome:?}");
        assert_eq!(outcome, Ok(Some(format!("Volume {percent}%"))));
    }

    /// Read-only: finds the Bluetooth radio and its state, changes nothing.
    #[test]
    #[ignore = "talks to the Bluetooth radio"]
    fn finds_the_bluetooth_radio() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let radios = Radio::GetRadiosAsync().unwrap().get().unwrap();
        let states: Vec<_> = (0..radios.Size().unwrap())
            .map(|ix| radios.GetAt(ix).unwrap())
            .map(|radio| (radio.Kind().unwrap(), radio.State().unwrap()))
            .collect();
        eprintln!("radios (kind, state): {states:?}");
        let access = Radio::RequestAccessAsync().unwrap().get().unwrap();
        eprintln!("access: {access:?}");
    }

    /// Toggles Bluetooth twice, so it ends as it started. Only run it when
    /// nothing is connected over Bluetooth.
    #[test]
    #[ignore = "switches the Bluetooth radio"]
    fn toggles_bluetooth_and_back() {
        let first = run_blocking(SystemCommand::ToggleBluetooth);
        let second = run_blocking(SystemCommand::ToggleBluetooth);
        eprintln!("bluetooth: {first:?}, then {second:?}");
        assert!(first.is_ok() && second.is_ok() && first != second);
    }

    /// Toggles HDR on the display under the pointer twice, so it ends as it
    /// started. The screen blinks twice.
    #[test]
    #[ignore = "switches HDR on a display"]
    fn toggles_hdr_and_back() {
        let before = hdr_status();
        let first = run_blocking(SystemCommand::ToggleHdr);
        let middle = hdr_status();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let second = run_blocking(SystemCommand::ToggleHdr);
        let after = hdr_status();
        eprintln!("hdr: {before:?} -> {first:?} -> {middle:?} -> {second:?} -> {after:?}");
        assert!(first.is_ok() && second.is_ok() && first != second);
        assert_eq!(before, after);
    }

    /// Read-only: asks which displays support HDR, changes nothing.
    #[test]
    fn reads_hdr_support() {
        let status = hdr_status();
        eprintln!(
            "HDR displays: {status:?}; under the pointer: {:?}",
            display_under_pointer()
        );
        assert!(status.is_ok());
    }
}
