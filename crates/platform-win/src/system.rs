//! System commands: power, session, Recycle Bin, theme, audio.

use std::{ffi::c_void, mem::size_of, os::windows::process::CommandExt, process, ptr, thread};

use spotlight_core::item::SystemCommand;
use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, LPARAM, WPARAM},
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole,
            eRender,
        },
        System::{
            Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx},
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
                HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
            },
        },
    },
    core::{PCWSTR, Result, w},
};

/// Runs `command` on a short-lived thread: some of these block (the Recycle Bin
/// shows its own confirmation and progress) or need COM.
pub fn run(command: SystemCommand) {
    thread::spawn(move || {
        if let Err(err) = run_blocking(command) {
            eprintln!("[system] {command:?} failed: {err}");
        }
    });
}

fn run_blocking(command: SystemCommand) -> Result<()> {
    unsafe {
        match command {
            SystemCommand::Lock => LockWorkStation(),
            SystemCommand::Sleep => {
                // Sleeps, or hibernates if the power settings turned sleep into hibernate.
                if SetSuspendState(false, false, false) {
                    Ok(())
                } else {
                    Err(windows::core::Error::from_win32())
                }
            }
            // `shutdown.exe` takes care of the shutdown privilege, which
            // `ExitWindowsEx` would make us enable first.
            SystemCommand::Restart => shutdown(&["/r", "/t", "0"]),
            SystemCommand::ShutDown => shutdown(&["/s", "/t", "0"]),
            SystemCommand::SignOut => ExitWindowsEx(
                EWX_LOGOFF,
                SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED,
            ),
            // No flags: Windows asks for confirmation and shows progress itself.
            SystemCommand::EmptyRecycleBin => SHEmptyRecycleBinW(None, PCWSTR::null(), 0),
            SystemCommand::ToggleDarkMode => toggle_dark_mode(),
            SystemCommand::ToggleMute => toggle_mute(),
        }
    }
}

fn shutdown(args: &[&str]) -> Result<()> {
    process::Command::new("shutdown.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map(drop)
        .map_err(|err| windows::core::Error::new(windows::core::HRESULT(-1), err.to_string()))
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

/// Flips apps and the system (taskbar, Start) between light and dark together,
/// following the apps setting, as the Settings app's single switch does.
unsafe fn toggle_dark_mode() -> Result<()> {
    unsafe {
        // The registry values say "light": 1 is light, 0 is dark.
        let next = u32::from(dark_mode());
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
                return Err(status.into());
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
        Ok(())
    }
}

/// Mutes or unmutes the default playback device.
unsafe fn toggle_mute() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        let muted = volume.GetMute()?.as_bool();
        volume.SetMute(!muted, ptr::null())
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
