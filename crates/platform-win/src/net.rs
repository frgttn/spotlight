//! HTTP through WinRT's `HttpClient`: Windows' own TLS and proxy settings,
//! no HTTP stack of our own.

use windows::{
    Foundation::Uri,
    Storage::Streams::DataReader,
    Web::Http::HttpClient,
    Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
    core::HSTRING,
};

/// Sites that turn away clients without one (Reddit) answer browsers.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Spotlight";

fn client() -> Result<HttpClient, String> {
    let client = HttpClient::new().map_err(|err| err.message())?;
    client
        .DefaultRequestHeaders()
        .and_then(|headers| headers.UserAgent())
        .and_then(|agent| agent.TryParseAdd(&HSTRING::from(USER_AGENT)))
        .map_err(|err| err.message())?;
    Ok(client)
}

/// Fetches `url` and returns the body as text. Blocking: call it from a
/// thread of its own.
pub fn get_text(url: &str) -> Result<String, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let uri = Uri::CreateUri(&HSTRING::from(url)).map_err(|err| err.message())?;
    let client = client()?;
    let body = client
        .GetStringAsync(&uri)
        .and_then(|operation| operation.get())
        .map_err(|err| format!("{url}: {}", err.message()))?;
    Ok(body.to_string_lossy())
}

/// Fetches `url` and returns the body, refusing more than `limit` bytes.
/// Blocking: call it from a thread of its own.
pub fn get_bytes(url: &str, limit: u32) -> Result<Vec<u8>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let uri = Uri::CreateUri(&HSTRING::from(url)).map_err(|err| err.message())?;
    let client = client()?;
    let fail = |err: windows::core::Error| format!("{url}: {}", err.message());
    let buffer = client
        .GetBufferAsync(&uri)
        .and_then(|operation| operation.get())
        .map_err(fail)?;
    let length = buffer.Length().map_err(fail)?;
    if length > limit {
        return Err(format!("{url}: {length} bytes is too big"));
    }
    let mut bytes = vec![0; length as usize];
    DataReader::FromBuffer(&buffer)
        .and_then(|reader| reader.ReadBytes(&mut bytes))
        .map_err(fail)?;
    Ok(bytes)
}
