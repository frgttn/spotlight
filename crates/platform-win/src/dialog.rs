//! Windows' own Open and Save dialogs, with a file type filter, which GPUI's
//! `prompt_for_paths` doesn't take. Each runs on a thread of its own (the
//! dialogs need a single-threaded COM apartment) and answers on a channel.

use std::{path::PathBuf, thread};

use windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FileOpenDialog, FileSaveDialog, IFileDialog,
            IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
            SIGDN_FILESYSPATH,
        },
    },
    core::{HSTRING, Interface, PCWSTR},
};

/// A file type the dialog offers: `("JSON Files", "*.json")`.
pub type FileType = (&'static str, &'static str);

/// What the dialog was asked: the window it belongs to (0 for none), its
/// title and the file types it offers, the first one picked at the start.
pub struct Request {
    pub owner: isize,
    pub title: String,
    pub types: Vec<FileType>,
}

/// Asks for a file to open. The channel gets the path, or `None` when the
/// dialog was cancelled.
pub fn open_file(request: Request) -> async_channel::Receiver<Result<Option<PathBuf>, String>> {
    run(move || unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        show(&dialog.cast()?, &request)
    })
}

/// Asks where to save a file, starting in `folder` with `name`; the first
/// file type's extension is added when the name has none.
pub fn save_file(
    request: Request,
    folder: PathBuf,
    name: String,
) -> async_channel::Receiver<Result<Option<PathBuf>, String>> {
    run(move || unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetFileName(&HSTRING::from(name.as_str()))?;
        if let Some((_, spec)) = request.types.first()
            && let Some(extension) = spec.strip_prefix("*.")
        {
            dialog.SetDefaultExtension(&HSTRING::from(extension))?;
        }
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
            &HSTRING::from(folder.as_os_str()),
            None,
        ) {
            let _ = dialog.SetFolder(&item);
        }
        show(&dialog.cast()?, &request)
    })
}

/// Sets the dialog up as `request` asks, shows it and reads the path picked.
unsafe fn show(dialog: &IFileDialog, request: &Request) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let names: Vec<HSTRING> = request
            .types
            .iter()
            .map(|(name, _)| (*name).into())
            .collect();
        let specs: Vec<HSTRING> = request
            .types
            .iter()
            .map(|(_, spec)| (*spec).into())
            .collect();
        let filters: Vec<COMDLG_FILTERSPEC> = names
            .iter()
            .zip(&specs)
            .map(|(name, spec)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(name.as_ptr()),
                pszSpec: PCWSTR(spec.as_ptr()),
            })
            .collect();
        if !filters.is_empty() {
            dialog.SetFileTypes(&filters)?;
            dialog.SetFileTypeIndex(1)?;
        }
        dialog.SetTitle(&HSTRING::from(request.title.as_str()))?;
        let owner = (request.owner != 0).then_some(HWND(request.owner as *mut _));
        // Cancelling is an error too.
        if dialog.Show(owner).is_err() {
            return Ok(None);
        }
        let raw = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = raw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(path)
    }
}

fn run(
    dialog: impl FnOnce() -> windows::core::Result<Option<PathBuf>> + Send + 'static,
) -> async_channel::Receiver<Result<Option<PathBuf>, String>> {
    let (tx, rx) = async_channel::bounded(1);
    let spawned = thread::Builder::new()
        .name("file-dialog".into())
        .spawn(move || {
            let result = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let result = dialog().map_err(|err| err.to_string());
                CoUninitialize();
                result
            };
            let _ = tx.send_blocking(result);
        });
    if let Err(err) = spawned {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(Err(err.to_string()));
        return rx;
    }
    rx
}
