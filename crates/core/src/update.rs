//! Where updating Spotlight has got, as Settings and the tray show it.

/// The state of the updater.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not started by the installer (a dev build or a copied exe), so there
    /// is nothing to update in place.
    Unmanaged,
    /// Not checked yet.
    Idle,
    Checking,
    UpToDate,
    /// Downloading this version.
    Downloading(String),
    /// This version is downloaded and replaces the running one on restart
    /// (or at the next start, if Spotlight isn't restarted for it).
    Ready(String),
    Failed(String),
}

impl Status {
    /// One line for Settings, after the running version.
    pub fn describe(&self) -> String {
        match self {
            Status::Unmanaged => {
                "Updates come with the installer; this copy wasn't installed".into()
            }
            Status::Idle => "Checks for updates automatically".into(),
            Status::Checking => "Checking for updates…".into(),
            Status::UpToDate => "Up to date".into(),
            Status::Downloading(version) => format!("Downloading v{version}…"),
            Status::Ready(version) => format!("v{version} is ready; restart to update"),
            Status::Failed(why) => format!("Couldn't check for updates: {why}"),
        }
    }

    /// Checking or downloading, so another check would only repeat it.
    pub fn busy(&self) -> bool {
        matches!(self, Status::Checking | Status::Downloading(_))
    }
}
