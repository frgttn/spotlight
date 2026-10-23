//! HTTP through WinRT's `HttpClient`: Windows' own TLS and proxy settings,
//! no HTTP stack of our own.

use windows::{
    Foundation::Uri,
    Web::Http::HttpClient,
    Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
    core::HSTRING,
};

/// Fetches `url` and returns the body as text. Blocking: call it from a
/// thread of its own.
pub fn get_text(url: &str) -> Result<String, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let uri = Uri::CreateUri(&HSTRING::from(url)).map_err(|err| err.message())?;
    let client = HttpClient::new().map_err(|err| err.message())?;
    let body = client
        .GetStringAsync(&uri)
        .and_then(|operation| operation.get())
        .map_err(|err| format!("{url}: {}", err.message()))?;
    Ok(body.to_string_lossy())
}
