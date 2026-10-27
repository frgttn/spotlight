//! Sites' icons, fetched from the sites themselves (see
//! [`spotlight_core::favicon`]) and decoded to the same BGRA as shell icons.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use image::imageops::FilterType;
use spotlight_core::favicon;

use crate::{icons::Icon, net};

/// Downloads at once; each waits on the network, not the CPU.
const WORKERS: usize = 4;
/// Bigger than any sensible icon: what is above is something else.
const MAX_BYTES: u32 = 1024 * 1024;

/// Fetches the icon of each `(key, origin)` on threads of their own and
/// sends it with its key. A site without a usable icon gets an empty one
/// (0×0), so it can be remembered as such.
pub fn spawn(sites: Vec<(usize, String)>, size: u32, icons: async_channel::Sender<(usize, Icon)>) {
    let sites = Arc::new(sites);
    let next = Arc::new(AtomicUsize::new(0));
    for worker in 0..WORKERS.min(sites.len()) {
        let (sites, next, icons) = (sites.clone(), next.clone(), icons.clone());
        let spawned = thread::Builder::new()
            .name(format!("favicons-{worker}"))
            .spawn(move || {
                while let Some((key, origin)) = sites.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let icon = fetch(origin, size).unwrap_or_else(|err| {
                        eprintln!("[favicons] none for {origin}: {err}");
                        Icon {
                            width: 0,
                            height: 0,
                            bgra: Vec::new(),
                        }
                    });
                    if icons.send_blocking((*key, icon)).is_err() {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            eprintln!("[favicons] could not start a thread: {err}");
        }
    }
}

/// The first icon of the site at `origin` that downloads and decodes,
/// `size` pixels square.
fn fetch(origin: &str, size: u32) -> Result<Icon, String> {
    // Without the page (it refuses us, say), `/favicon.ico` is still tried.
    let html = net::get_text(origin).unwrap_or_default();
    let mut last = String::from("no icon");
    for url in favicon::candidates(&html, origin, size) {
        match net::get_bytes(&url, MAX_BYTES).and_then(|bytes| decode(&bytes, size)) {
            Ok(icon) => return Ok(icon),
            Err(err) => last = err,
        }
    }
    Err(last)
}

/// Any format the `image` crate reads, scaled to fit `size` square and
/// centred on a transparent square.
fn decode(bytes: &[u8], size: u32) -> Result<Icon, String> {
    let image = image::load_from_memory(bytes).map_err(|err| err.to_string())?;
    let scaled = image.resize(size, size, FilterType::Lanczos3).to_rgba8();
    let mut square = image::RgbaImage::new(size, size);
    let x = (size - scaled.width()) / 2;
    let y = (size - scaled.height()) / 2;
    image::imageops::overlay(&mut square, &scaled, x.into(), y.into());
    let mut bgra = square.into_raw();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Icon {
        width: size,
        height: size,
        bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_to_a_bgra_square() {
        let mut wide = image::RgbaImage::new(4, 2);
        for pixel in wide.pixels_mut() {
            *pixel = image::Rgba([255, 0, 0, 255]);
        }
        let mut png = Vec::new();
        wide.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let icon = decode(&png, 8).unwrap();
        assert_eq!((icon.width, icon.height), (8, 8));
        // Letterboxed: transparent top row, red (as BGRA) in the middle.
        assert_eq!(&icon.bgra[..4], &[0, 0, 0, 0]);
        let middle = (4 * 8 + 4) * 4;
        assert_eq!(&icon.bgra[middle..middle + 4], &[0, 0, 255, 255]);
        assert!(decode(b"not an image", 8).is_err());
    }
}
