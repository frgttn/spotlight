//! The service's place in Windows: installing and removing it (from an
//! elevated `spotlight-service.exe install`), and running as one under the
//! Service Control Manager.
//!
//! Installing copies the executable to `%ProgramFiles%\Spotlight`, so what
//! runs as LocalSystem is a file only administrators can replace, and keeps
//! its data in `%ProgramData%\Spotlight`, for SYSTEM and Administrators only.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant},
};

use spotlight_platform_win::{index_service, ntfs};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_SERVICE_ALREADY_RUNNING, HANDLE, HLOCAL, LocalFree},
        Security::{
            ACL,
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                SE_FILE_OBJECT, SetNamedSecurityInfoW,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
        System::{
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
                RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
            },
            Services::{
                ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
                CreateServiceW, DeleteService, ENUM_SERVICE_TYPE, OpenSCManagerW, OpenServiceW,
                QueryServiceStatus, RegisterServiceCtrlHandlerExW, SC_ACTION, SC_ACTION_RESTART,
                SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
                SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_FAILURE_ACTIONS,
                SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO,
                SERVICE_DESCRIPTIONW, SERVICE_ERROR, SERVICE_ERROR_NORMAL,
                SERVICE_FAILURE_ACTIONSW, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
                SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
                SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
                StartServiceCtrlDispatcherW, StartServiceW,
            },
            Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject},
        },
    },
    core::{HSTRING, PCWSTR, PWSTR},
};

use crate::{
    log,
    server::{Server, Volumes},
    store::Store,
    volume::Source,
};

const DISPLAY_NAME: &str = "Spotlight File Index";
const DESCRIPTION: &str = "Keeps Spotlight's file search current from the NTFS change journal. Only Spotlight can ask it, and only about folders its user can open.";
/// Under the service's registry key: the Spotlight executables allowed to
/// connect, one path per line.
const PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\SpotlightIndex\Parameters";
const CLIENTS_VALUE: &str = "Clients";
/// SYSTEM and Administrators only, not inherited from `%ProgramData%`.
const DATA_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

pub fn install_dir() -> PathBuf {
    PathBuf::from(env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into()))
        .join("Spotlight")
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()))
        .join("Spotlight")
}

/// Installs (or updates) the service and starts it. `client` is the
/// Spotlight executable allowed to connect. Needs administrator rights.
pub fn install(client: &Path) -> Result<(), String> {
    let source = env::current_exe().map_err(|err| err.to_string())?;
    let target_dir = install_dir();
    let target = target_dir.join("spotlight-service.exe");
    let data = data_dir();
    fs::create_dir_all(&data).map_err(|err| format!("{}: {err}", data.display()))?;
    secure(&data)?;
    unsafe {
        let manager = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)
            .map_err(|err| format!("Service manager: {err}"))?;
        let existing = OpenServiceW(
            manager,
            &HSTRING::from(index_service::SERVICE),
            SERVICE_ALL_ACCESS,
        )
        .ok();
        if let Some(service) = existing {
            stop(service);
        }
        if source != target {
            fs::create_dir_all(&target_dir)
                .map_err(|err| format!("{}: {err}", target_dir.display()))?;
            fs::copy(&source, &target)
                .map_err(|err| format!("Couldn't copy to {}: {err}", target.display()))?;
        }
        let command = HSTRING::from(format!("\"{}\" run", target.display()));
        let service = match existing {
            Some(service) => {
                ChangeServiceConfigW(
                    service,
                    ENUM_SERVICE_TYPE(SERVICE_WIN32_OWN_PROCESS.0),
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    &command,
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    &HSTRING::from(DISPLAY_NAME),
                )
                .map_err(|err| format!("Couldn't update the service: {err}"))?;
                service
            }
            None => CreateServiceW(
                manager,
                &HSTRING::from(index_service::SERVICE),
                &HSTRING::from(DISPLAY_NAME),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR(SERVICE_ERROR_NORMAL.0),
                &command,
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                // LocalSystem.
                PCWSTR::null(),
                PCWSTR::null(),
            )
            .map_err(|err| format!("Couldn't create the service: {err}"))?,
        };
        configure(service);
        set_clients(&[client.to_path_buf()])?;
        let started = StartServiceW(service, None);
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
        match started {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => Ok(()),
            Err(err) => Err(format!("Couldn't start the service: {err}")),
        }
    }
}

/// Stops and removes the service, its copy and its data. Needs
/// administrator rights.
pub fn uninstall() -> Result<(), String> {
    unsafe {
        let manager = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS)
            .map_err(|err| format!("Service manager: {err}"))?;
        if let Ok(service) = OpenServiceW(
            manager,
            &HSTRING::from(index_service::SERVICE),
            SERVICE_ALL_ACCESS,
        ) {
            stop(service);
            let deleted = DeleteService(service);
            let _ = CloseServiceHandle(service);
            deleted.map_err(|err| format!("Couldn't remove the service: {err}"))?;
        }
        let _ = CloseServiceHandle(manager);
    }
    let _ = fs::remove_dir_all(data_dir());
    let target = install_dir().join("spotlight-service.exe");
    if env::current_exe().ok().as_deref() != Some(target.as_path()) {
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir(install_dir());
    }
    Ok(())
}

/// Asks a running service to stop and waits for it, up to 15 seconds.
unsafe fn stop(service: SC_HANDLE) {
    unsafe {
        let mut status = SERVICE_STATUS::default();
        let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if QueryServiceStatus(service, &mut status).is_err()
                || status.dwCurrentState == SERVICE_STOPPED
            {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Description, delayed start (it isn't needed during boot) and restarts
/// after a crash.
unsafe fn configure(service: SC_HANDLE) {
    unsafe {
        let description = HSTRING::from(DESCRIPTION);
        let info = SERVICE_DESCRIPTIONW {
            lpDescription: PWSTR(description.as_ptr() as *mut u16),
        };
        let _ = ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_DESCRIPTION,
            Some(&info as *const _ as *const _),
        );
        let delayed = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: true.into(),
        };
        let _ = ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            Some(&delayed as *const _ as *const _),
        );
        let mut actions = [SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5_000,
        }; 3];
        let failure = SERVICE_FAILURE_ACTIONSW {
            dwResetPeriod: 24 * 3600,
            cActions: actions.len() as u32,
            lpsaActions: actions.as_mut_ptr(),
            ..Default::default()
        };
        let _ = ChangeServiceConfig2W(
            service,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some(&failure as *const _ as *const _),
        );
    }
}

/// Gives `dir` the [`DATA_SDDL`] access rules.
fn secure(dir: &Path) -> Result<(), String> {
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(DATA_SDDL),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|err| err.to_string())?;
        let mut present = false.into();
        let mut defaulted = false.into();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let got = GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted);
        let set = got.map_err(|err| err.to_string()).and_then(|()| {
            SetNamedSecurityInfoW(
                &HSTRING::from(dir.as_os_str()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
            .ok()
            .map_err(|err| format!("Couldn't restrict {}: {err}", dir.display()))
        });
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        set
    }
}

fn set_clients(clients: &[PathBuf]) -> Result<(), String> {
    let text = clients
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    let value: Vec<u16> = text.encode_utf16().chain([0]).collect();
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(PARAMETERS),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .map_err(|err| format!("Couldn't save the allowed clients: {err}"))?;
        let bytes = std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2);
        let set = RegSetValueExW(
            key,
            &HSTRING::from(CLIENTS_VALUE),
            None,
            REG_SZ,
            Some(bytes),
        );
        let _ = RegCloseKey(key);
        set.ok()
            .map_err(|err| format!("Couldn't save the allowed clients: {err}"))
    }
}

fn clients() -> Vec<PathBuf> {
    let mut buffer = vec![0u16; 8192];
    let mut size = (buffer.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(PARAMETERS),
            &HSTRING::from(CLIENTS_VALUE),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read.is_err() {
        return Vec::new();
    }
    let _ = KEY_READ;
    let len = (size as usize / 2).saturating_sub(1);
    String::from_utf16_lossy(&buffer[..len])
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Everything the running service holds, started by [`serve`].
struct Running {
    server: Arc<Server>,
    volumes: Arc<Volumes>,
}

/// Starts serving: the store, the volumes, and the pipe on a thread.
fn serve() -> Result<Running, String> {
    let data = data_dir();
    fs::create_dir_all(&data).map_err(|err| err.to_string())?;
    let store = match Store::open(&data.join("index")) {
        Ok(store) => Some(Arc::new(store)),
        Err(err) => {
            log!("no snapshots this run: {err}");
            None
        }
    };
    let volumes = Arc::new(Volumes::new(
        |letter| Ok(Box::new(ntfs::Volume::open(letter)?) as Box<dyn Source>),
        store,
    ));
    let clients = clients();
    log!("serving {} allowed client(s)", clients.len());
    let server = Arc::new(Server::new(index_service::PIPE, &clients, volumes.clone()));
    let serving = server.clone();
    thread::Builder::new()
        .name("pipe".into())
        .spawn(move || serving.serve())
        .map_err(|err| err.to_string())?;
    Ok(Running { server, volumes })
}

/// Runs in the console until Ctrl+C, logging to stderr: for trying it out
/// from an elevated terminal.
pub fn console() -> Result<(), String> {
    let _running = serve()?;
    log!("serving on {}; Ctrl+C stops", index_service::PIPE);
    loop {
        thread::park();
    }
}

/// The service's stop event, set by the control handler.
static STOP: OnceLock<isize> = OnceLock::new();
static STATUS: OnceLock<isize> = OnceLock::new();

/// Hands the thread to the Service Control Manager, which calls
/// `service_main`. Returns when the service has stopped.
pub fn run_as_service() -> Result<(), String> {
    let name = HSTRING::from(index_service::SERVICE);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_ptr() as *mut u16),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .map_err(|err| format!("Not started by the Service Control Manager: {err}"))
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    crate::log::to_file(&data_dir().join("service.log"));
    unsafe {
        let Ok(stop) = CreateEventW(None, true, false, None) else {
            return;
        };
        let _ = STOP.set(stop.0 as isize);
        let Ok(status) = RegisterServiceCtrlHandlerExW(
            &HSTRING::from(index_service::SERVICE),
            Some(control),
            None,
        ) else {
            return;
        };
        let _ = STATUS.set(status.0 as isize);
        report(SERVICE_START_PENDING);
        match serve() {
            Ok(running) => {
                report(SERVICE_RUNNING);
                log!("started");
                WaitForSingleObject(stop, INFINITE);
                report(SERVICE_STOP_PENDING);
                running.server.stop();
                running.volumes.shut_down();
                log!("stopped");
            }
            Err(err) => log!("couldn't start: {err}"),
        }
        let _ = CloseHandle(stop);
        report(SERVICE_STOPPED);
    }
}

unsafe extern "system" fn control(
    code: u32,
    _kind: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if (code == SERVICE_CONTROL_STOP || code == SERVICE_CONTROL_SHUTDOWN)
        && let Some(&stop) = STOP.get()
    {
        unsafe {
            let _ = SetEvent(HANDLE(stop as _));
        }
    }
    0
}

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let Some(&handle) = STATUS.get() else {
        return;
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWaitHint: 10_000,
        ..Default::default()
    };
    unsafe {
        let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(handle as _), &status);
    }
}
