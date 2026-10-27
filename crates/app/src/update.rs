//! Installing, updating and removing Spotlight through Velopack.
//!
//! The installer (`Spotlight-win-Setup.exe`, made by `vpk pack` in
//! `.github/workflows/release.yml`) puts Spotlight in
//! `%LocalAppData%\Spotlight\current`, which stays the same path across
//! updates, so start at login, the `spotlight://` links and the index
//! service's allowed client keep pointing at it.
//!
//! Updates come from the repository's GitHub Releases: a check a minute
//! after start and then every few hours downloads a newer version in the
//! background. It replaces the running one when the user picks "Restart to
//! Update" (tray or Settings › About), or at the next start otherwise.

use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use spotlight_core::{deeplink, update::Status};
use spotlight_platform_win::{autostart, index_service, protocol};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp, sources::GithubSource};

/// The first check, after start has settled.
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Handles the installer's calls (`--veloapp-install` and the like), which
/// exit when done, and applies an update downloaded earlier. Runs first in
/// `main`, before anything else starts.
pub fn run_installer_hooks() {
    VelopackApp::build()
        // Like Raycast, Spotlight starts at login once installed; the tray
        // and Settings turn it off.
        .on_after_install_fast_callback(|_| {
            if let Err(err) = autostart::set_enabled(true) {
                eprintln!("[update] could not turn on start at login: {err}");
            }
        })
        // The installer removes the program; what Spotlight registered
        // outside it goes too. The data in %APPDATA% stays, as with most
        // apps, in case Spotlight is installed again.
        .on_before_uninstall_fast_callback(|_| {
            if autostart::is_enabled()
                && let Err(err) = autostart::set_enabled(false)
            {
                eprintln!("[update] could not turn off start at login: {err}");
            }
            if let Err(err) = protocol::unregister(deeplink::SCHEME) {
                eprintln!("[update] {err}");
            }
            // It runs as LocalSystem from %ProgramFiles%, so removing it
            // takes the UAC prompt; declined, it stays until removed from an
            // elevated `spotlight-service.exe uninstall`.
            if index_service::state() != index_service::State::NotInstalled
                && let Err(err) = index_service::uninstall()
            {
                eprintln!("[update] could not remove the index service: {err}");
            }
        })
        .run();
}

/// Checks for, downloads and applies updates. Shared between the checking
/// thread and the UI thread.
pub struct Updater {
    /// `None` when Spotlight wasn't installed by the installer.
    manager: Option<UpdateManager>,
    status: Mutex<Status>,
    /// The downloaded update, once there is one.
    ready: Mutex<Option<UpdateInfo>>,
    /// Told whenever `status` changes.
    changed: async_channel::Sender<()>,
}

impl Updater {
    pub fn new(changed: async_channel::Sender<()>) -> Arc<Self> {
        let source = GithubSource::new(env!("CARGO_PKG_REPOSITORY"), None, false);
        let manager = UpdateManager::new(source, None, None).ok();
        let status = if manager.is_some() {
            Status::Idle
        } else {
            Status::Unmanaged
        };
        Arc::new(Self {
            manager,
            status: Mutex::new(status),
            ready: Mutex::new(None),
            changed,
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Checks now and every few hours after, on a thread of its own.
    pub fn check_periodically(self: &Arc<Self>) {
        if self.manager.is_none() {
            return;
        }
        let updater = self.clone();
        let spawned = thread::Builder::new()
            .name("updates".into())
            .spawn(move || {
                thread::sleep(FIRST_CHECK);
                loop {
                    updater.check_blocking();
                    thread::sleep(CHECK_EVERY);
                }
            });
        if let Err(err) = spawned {
            eprintln!("[update] could not start checking for updates: {err}");
        }
    }

    /// Checks once, on a thread of its own (from Settings' "Check now").
    pub fn check(self: &Arc<Self>) {
        if self.manager.is_none() || self.status().busy() {
            return;
        }
        let updater = self.clone();
        let spawned = thread::Builder::new()
            .name("update-check".into())
            .spawn(move || updater.check_blocking());
        if let Err(err) = spawned {
            eprintln!("[update] could not check for updates: {err}");
        }
    }

    /// Quits and starts the downloaded version, if there is one.
    pub fn restart(&self) {
        let (Some(manager), Some(update)) = (&self.manager, self.ready.lock().unwrap().clone())
        else {
            return;
        };
        // Exits this process; the updater waits for it, swaps the files and
        // starts the new version.
        if let Err(err) = manager.apply_updates_and_restart(&update) {
            eprintln!("[update] could not apply the update: {err}");
            self.set(Status::Failed(err.to_string()));
        }
    }

    fn check_blocking(&self) {
        let Some(manager) = &self.manager else { return };
        // A downloaded update stays the one to restart into; Velopack applies
        // it at the next start anyway.
        if self.ready.lock().unwrap().is_some() || self.status().busy() {
            return;
        }
        self.set(Status::Checking);
        let update = match manager.check_for_updates() {
            Ok(UpdateCheck::UpdateAvailable(update)) => *update,
            Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => {
                self.set(Status::UpToDate);
                return;
            }
            Err(err) => {
                eprintln!("[update] checking failed: {err}");
                self.set(Status::Failed(err.to_string()));
                return;
            }
        };
        let version = update.TargetFullRelease.Version.clone();
        self.set(Status::Downloading(version.clone()));
        if let Err(err) = manager.download_updates(&update, None) {
            eprintln!("[update] downloading v{version} failed: {err}");
            self.set(Status::Failed(err.to_string()));
            return;
        }
        eprintln!("[update] v{version} downloaded; it applies on restart");
        *self.ready.lock().unwrap() = Some(update);
        self.set(Status::Ready(version));
    }

    fn set(&self, status: Status) {
        *self.status.lock().unwrap() = status;
        let _ = self.changed.send_blocking(());
    }
}
